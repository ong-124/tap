// Copyright (c) 2023-present, Raphael Amorim.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

use rio_backend::config::input_bar::SizeSpec;
use rio_backend::sugarloaf::text::DrawOpts;
use rio_backend::sugarloaf::Sugarloaf;
use std::time::Instant;

/// Convert `[f32; 4]` colour to the `[u8; 4]` non-premul form the
/// `Text` draw path expects (the vertex shader premultiplies).
#[inline]
fn color_u8(c: [f32; 4]) -> [u8; 4] {
    [
        (c[0].clamp(0.0, 1.0) * 255.0) as u8,
        (c[1].clamp(0.0, 1.0) * 255.0) as u8,
        (c[2].clamp(0.0, 1.0) * 255.0) as u8,
        (c[3].clamp(0.0, 1.0) * 255.0) as u8,
    ]
}

// Layout: the bar is sized relative to the terminal — six terminal
// lines tall, text at twice the terminal font size. Metrics are set
// from Screen::new / update_config / change_font_size via
// `update_metrics`.
pub const INPUT_BAR_LINES: f32 = 6.0;
pub const INPUT_FONT_SCALE: f32 = 2.0;

const PANEL_INSET_X: f32 = 8.0;
const PANEL_INSET_Y: f32 = 4.0;
const TEXT_PADDING_X: f32 = 12.0;
const PROMPT_GAP: f32 = 10.0;
/// Floor for the configured height — a bar shorter than this cannot
/// fit its own text.
const MIN_BAR_HEIGHT: f32 = 24.0;

const CARET_WIDTH: f32 = 2.0;
/// Horizontal gap between entries in the Tab-completion candidates row.
const CANDIDATE_GAP: f32 = 14.0;
/// Blink period shared by the render-time phase math and the wake the
/// Screen re-arms after every frame.
pub const CARET_BLINK_MS: u128 = 500;
const PREEDIT_UNDERLINE_HEIGHT: f32 = 2.0;

/// Kept in memory only, mirroring the search history limit.
pub const MAX_HISTORY: usize = 255;

const PROMPT_SYMBOL: &str = "\u{276F}";
const LOCKED_HINT: &str = "keys pass through";

// Colors. The panel itself shares the window background; only the
// thick border ring is the bar's own paint. Shared with the pane
// frames (focused pane / unlocked bar = strong, rest = dim).
pub(crate) const BORDER_COLOR: [f32; 4] = [0.85, 0.85, 0.85, 0.32];
pub(crate) const LOCKED_BORDER_COLOR: [f32; 4] = [0.55, 0.55, 0.55, 0.14];
const TEXT_COLOR: [f32; 4] = [0.93, 0.93, 0.93, 1.0];
const DIM_TEXT_COLOR: [f32; 4] = [0.50, 0.50, 0.50, 1.0];
const PROMPT_COLOR: [f32; 4] = [0.42, 0.78, 0.42, 1.0];
const PREEDIT_TEXT_COLOR: [f32; 4] = [0.70, 0.70, 0.75, 1.0];

// Depth / order — shares the overlay band with search / palette.
const DEPTH_BG: f32 = 0.1;
const DEPTH_ELEMENT: f32 = 0.2;
const ORDER: u8 = 20;

/// Warp-style unified input bar pinned to the bottom of the window.
/// Keyboard input lands here while the focused pane sits at a shell
/// prompt (OSC 133 zone); a full line is written to the PTY on Enter.
/// While a command runs the bar is locked and keys pass straight to
/// the terminal.
pub struct InputBar {
    enabled: bool,
    locked: bool,
    /// Input text size in logical px (INPUT_FONT_SCALE × terminal).
    font_size: f32,
    /// `[input-bar] width` — resolved against the window at render
    /// time, the panel is centered horizontally.
    width: SizeSpec,
    /// `[input-bar] height` — `None` falls back to six terminal lines.
    height_spec: Option<SizeSpec>,
    /// One terminal line in logical px, for the Lines fallback.
    cell_height: f32,
    /// Reserved height in logical px, resolved from `height_spec`.
    height_px: f32,
    buffer: String,
    /// Cursor position in chars.
    cursor: usize,
    /// First visible char when the line overflows the bar.
    view_start: usize,
    history: Vec<String>,
    /// History slot being previewed, `None` for the live line.
    history_pos: Option<usize>,
    /// The live line, stashed while previewing history.
    draft: String,
    preedit: Option<String>,
    /// Ghost suggestion (zsh-autosuggestions style): the suffix of the
    /// most recent history entry matching the buffer as a prefix,
    /// rendered dim after the caret and accepted with → / End.
    suggestion: Option<String>,
    /// Live Tab-completion cycle state; kept only while the word under
    /// the cursor is still the one this state last produced.
    completion: Option<CompletionState>,
    /// `[input-bar] record-history` — persist submitted lines.
    record_history: bool,
    /// `[input-bar] suggestions` — ghost-text suggestions on/off.
    suggestions: bool,
    caret_blink_start: Instant,
}

/// Tab-completion cycle: the word region it owns, the word currently
/// in the buffer, the candidate list and which candidate applies on
/// the next Tab press.
struct CompletionState {
    /// Char index where the completed word begins.
    word_start: usize,
    /// Word text currently in the buffer (LCP or last candidate).
    current: String,
    candidates: Vec<crate::completion::Candidate>,
    /// Candidate applied when Tab cycles next.
    index: usize,
}

impl Default for InputBar {
    fn default() -> Self {
        Self {
            enabled: false,
            locked: false,
            font_size: 0.0,
            width: SizeSpec::Percent(100.0),
            height_spec: None,
            cell_height: 0.0,
            height_px: 0.0,
            buffer: String::new(),
            cursor: 0,
            view_start: 0,
            history: Vec::new(),
            history_pos: None,
            draft: String::new(),
            preedit: None,
            suggestion: None,
            completion: None,
            record_history: true,
            suggestions: true,
            caret_blink_start: Instant::now(),
        }
    }
}

impl InputBar {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            ..Self::default()
        }
    }

    #[inline]
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    #[inline]
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Height the terminal layout must reserve below the grid.
    #[inline]
    pub fn height(&self) -> f32 {
        if self.enabled {
            self.height_px
        } else {
            0.0
        }
    }

    /// Pure metrics setter (testable without a GPU): `font_size` is
    /// the terminal's, `cell_height` one terminal line in logical px.
    pub fn set_metrics(&mut self, font_size: f32, cell_height: f32) {
        self.font_size = font_size * INPUT_FONT_SCALE;
        self.cell_height = cell_height;
        self.resolve_height(0.0);
    }

    /// Apply `[input-bar] width` / `height`.
    pub fn set_size(&mut self, width: SizeSpec, height: Option<SizeSpec>) {
        self.width = width;
        self.height_spec = height;
    }

    /// Resolve the reserved height from `height_spec`: pixels as-is,
    /// percent of the window height (with a floor so a stray config
    /// cannot erase the bar), six terminal lines when unset.
    fn resolve_height(&mut self, window_height: f32) {
        self.height_px = match self.height_spec {
            None => self.cell_height * INPUT_BAR_LINES,
            Some(SizeSpec::Px(px)) => px,
            Some(SizeSpec::Percent(percent)) => {
                (percent / 100.0 * window_height).max(MIN_BAR_HEIGHT)
            }
        }
        .max(MIN_BAR_HEIGHT);
    }

    /// Re-resolve after a window resize (percent heights track the
    /// window). Returns whether the reserved height changed.
    pub fn refresh_height(&mut self, window_height: f32) -> bool {
        let before = self.height_px;
        self.resolve_height(window_height);
        before != self.height_px
    }

    /// Panel geometry inside the reserved band: width resolved from
    /// the config and centered horizontally.
    fn panel_geometry(&self, logical_width: f32) -> (f32, f32) {
        let available = (logical_width - PANEL_INSET_X * 2.0).max(0.0);
        let width = match self.width {
            SizeSpec::Px(px) => px,
            SizeSpec::Percent(percent) => percent / 100.0 * logical_width,
        }
        .clamp(0.0, available);
        (PANEL_INSET_X + (available - width) / 2.0, width)
    }

    /// Recompute size from the terminal's live font: the bar is six
    /// terminal lines tall with twice the terminal font size.
    pub fn update_metrics(
        &mut self,
        sugarloaf: &Sugarloaf,
        font_size: f32,
        line_height: f32,
    ) {
        let scale = sugarloaf.scale_factor();
        let (_, cell) = sugarloaf.compute_cell_metrics(font_size, line_height, scale);
        self.font_size = font_size * INPUT_FONT_SCALE;
        self.cell_height = cell.cell_height as f32 / scale;
        let window = sugarloaf.window_size();
        self.resolve_height(window.height / scale);
    }

    /// Update the lock state from the focused pane's zone. Locking
    /// drops suggestion/completion state (a half-composed suggestion
    /// no longer matches the prompt on return); unlocking restarts the
    /// blink so the caret is immediately visible again.
    pub fn set_locked(&mut self, locked: bool) {
        if self.locked != locked {
            self.locked = locked;
            if locked {
                self.suggestion = None;
                self.completion = None;
            } else {
                self.caret_blink_start = Instant::now();
            }
        }
    }

    #[inline]
    pub fn has_preedit(&self) -> bool {
        self.preedit.is_some()
    }

    #[inline]
    pub fn preedit_text(&self) -> Option<String> {
        self.preedit.clone()
    }

    pub fn set_preedit(&mut self, preedit: Option<String>) {
        if self.preedit != preedit {
            self.preedit = preedit;
            // Keep the caret solid while composing, like the terminal
            // cursor during preedit.
            self.caret_blink_start = Instant::now();
            self.invalidate_completion();
            self.refresh_suggestion();
        }
    }

    /// Replace the browsable history (startup load of the persistent
    /// store). Any in-flight browse resets to the live line.
    pub fn set_history(&mut self, history: Vec<String>) {
        self.history = history;
        self.history_pos = None;
        self.draft.clear();
        self.invalidate_completion();
        self.refresh_suggestion();
    }

    /// `[input-bar] record-history` / `suggestions`.
    pub fn set_options(&mut self, record_history: bool, suggestions: bool) {
        self.record_history = record_history;
        self.suggestions = suggestions;
        self.invalidate_completion();
        self.refresh_suggestion();
    }

    /// Whether submissions should reach the persistent store.
    #[inline]
    pub fn records_history(&self) -> bool {
        self.record_history
    }

    /// Whether the last submit actually entered the history list
    /// (adjacent dedup may have skipped it); the caller uses this to
    /// keep the persistent store in lockstep.
    #[inline]
    pub fn last_history_is(&self, line: &str) -> bool {
        self.history.last().map(String::as_str) == Some(line)
    }

    fn invalidate_completion(&mut self) {
        self.completion = None;
    }

    /// Recompute the ghost suggestion from the tail of the history:
    /// most recent entry matching the buffer as a prefix wins. Only
    /// meaningful at end-of-line, outside composition and history
    /// browsing — otherwise `None`.
    fn refresh_suggestion(&mut self) {
        let suffix = if self.suggestions
            && self.preedit.is_none()
            && self.history_pos.is_none()
            && self.cursor == self.char_count()
            && !self.buffer.is_empty()
        {
            let buffer = self.buffer.clone();
            self.history
                .iter()
                .rev()
                .find(|entry| entry.starts_with(&buffer) && entry.as_str() != buffer)
                .map(|entry| entry[buffer.len()..].to_string())
        } else {
            None
        };
        self.suggestion = suffix;
    }

    /// → / End at end-of-line: append the ghost suffix. Returns
    /// whether a suggestion was consumed; the caller falls back to the
    /// plain cursor movement otherwise.
    pub fn accept_suggestion(&mut self) -> bool {
        let Some(suggestion) = self.suggestion.take() else {
            return false;
        };
        self.fork_from_history();
        self.buffer.push_str(&suggestion);
        self.cursor = self.char_count();
        self.view_start = 0;
        self.invalidate_completion();
        self.refresh_suggestion();
        true
    }

    /// Tab: complete the word before the cursor. First press inserts
    /// the common prefix (or the single candidate plus a `/` dir /
    /// space suffix); further presses on an unchanged word cycle the
    /// full candidates. An empty word or no matches consumes the key
    /// without acting.
    pub fn tab_complete(&mut self, sources: &crate::completion::Sources) {
        if self.preedit.is_some() {
            return;
        }

        let (word_start, word) = self.word_before_cursor();

        // Cycle: same word start and same word text as the live state.
        if let Some(state) = self.completion.take() {
            if state.word_start == word_start
                && state.current == word
                && !state.candidates.is_empty()
            {
                let candidate = &state.candidates[state.index];
                let insert = candidate.insert.clone();
                self.apply_word(word_start, &insert);
                self.completion = Some(CompletionState {
                    index: (state.index + 1) % state.candidates.len(),
                    current: insert,
                    ..state
                });
                self.refresh_suggestion();
                return;
            }
            // Stale state: dropped, a fresh completion runs below.
        }

        if word.is_empty() {
            return;
        }

        let candidates = crate::completion::gather(word_start, &word, sources);
        match candidates.len() {
            0 => {}
            1 => {
                let mut insert = candidates[0].insert.clone();
                if !candidates[0].is_dir {
                    insert.push(' ');
                }
                self.apply_word(word_start, &insert);
                self.refresh_suggestion();
            }
            _ => {
                let inserts: Vec<&str> =
                    candidates.iter().map(|c| c.insert.as_str()).collect();
                let prefix = crate::completion::common_prefix(&inserts);
                if prefix.chars().count() > word.chars().count() {
                    self.apply_word(word_start, &prefix);
                }
                self.completion = Some(CompletionState {
                    word_start,
                    current: prefix,
                    candidates,
                    index: 0,
                });
                self.refresh_suggestion();
            }
        }
    }

    /// Replace `[word_start, cursor)` with `word`, cursor ending at
    /// the word's end.
    fn apply_word(&mut self, word_start: usize, word: &str) {
        self.fork_from_history();
        let from = self.byte_index(word_start);
        let to = self.byte_index(self.cursor);
        self.buffer.replace_range(from..to, word);
        self.cursor = word_start + word.chars().count();
        self.view_start = 0;
    }

    /// The whitespace-delimited word ending at the cursor: its first
    /// char index and text. Empty when the cursor sits at a word start.
    fn word_before_cursor(&self) -> (usize, String) {
        let chars: Vec<char> = self.buffer.chars().collect();
        let cursor = self.cursor.min(chars.len());
        let mut start = cursor;
        while start > 0 && !chars[start - 1].is_whitespace() {
            start -= 1;
        }
        (start, chars[start..cursor].iter().collect())
    }

    fn char_count(&self) -> usize {
        self.buffer.chars().count()
    }

    fn byte_index(&self, char_index: usize) -> usize {
        self.buffer
            .char_indices()
            .nth(char_index)
            .map(|(i, _)| i)
            .unwrap_or(self.buffer.len())
    }

    /// Editing a previewed history entry leaves the history browser:
    /// the edited copy becomes the live line.
    fn fork_from_history(&mut self) {
        if self.history_pos.is_some() {
            self.history_pos = None;
        }
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
        self.view_start = 0;
        self.history_pos = None;
        self.draft.clear();
        self.preedit = None;
        self.invalidate_completion();
        self.refresh_suggestion();
        self.caret_blink_start = Instant::now();
    }

    /// Append text at the cursor (typing, paste, IME commit). Control
    /// characters are dropped; line breaks and tabs become spaces so a
    /// pasted block stays one editable line.
    pub fn insert_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let sanitized: String = text
            .chars()
            .map(|c| match c {
                '\n' | '\r' | '\t' => ' ',
                c if c.is_control() => '\u{0}',
                c => c,
            })
            .filter(|c| *c != '\u{0}')
            .collect();
        if sanitized.is_empty() {
            return;
        }
        self.fork_from_history();
        let byte = self.byte_index(self.cursor);
        self.buffer.insert_str(byte, &sanitized);
        self.cursor += sanitized.chars().count();
        self.invalidate_completion();
        self.refresh_suggestion();
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.fork_from_history();
        let start = self.byte_index(self.cursor - 1);
        let end = self.byte_index(self.cursor);
        self.buffer.drain(start..end);
        self.cursor -= 1;
        self.invalidate_completion();
        self.refresh_suggestion();
    }

    pub fn delete_forward(&mut self) {
        if self.cursor >= self.char_count() {
            return;
        }
        self.fork_from_history();
        let start = self.byte_index(self.cursor);
        let end = self.byte_index(self.cursor + 1);
        self.buffer.drain(start..end);
        self.invalidate_completion();
        self.refresh_suggestion();
    }

    /// Ctrl+W: bash-style rubout — kill backwards the word before the
    /// cursor and the whitespace separating it from what remains,
    /// leaving `foo bar baz` as `foo bar`.
    pub fn delete_word_backwards(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.fork_from_history();
        let chars: Vec<char> = self.buffer.chars().collect();
        let mut start = self.cursor;
        // The word itself.
        while start > 0 && !chars[start - 1].is_whitespace() {
            start -= 1;
        }
        // The whitespace run before it.
        while start > 0 && chars[start - 1].is_whitespace() {
            start -= 1;
        }
        if start == self.cursor {
            return;
        }
        let from = self.byte_index(start);
        let to = self.byte_index(self.cursor);
        self.buffer.drain(from..to);
        self.cursor = start;
        self.invalidate_completion();
        self.refresh_suggestion();
    }

    /// Ctrl+U: delete from the line start to the cursor.
    pub fn delete_to_line_start(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.fork_from_history();
        let end = self.byte_index(self.cursor);
        self.buffer.drain(..end);
        self.cursor = 0;
        self.view_start = 0;
        self.invalidate_completion();
        self.refresh_suggestion();
    }

    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
        self.invalidate_completion();
        self.refresh_suggestion();
    }

    pub fn move_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.char_count());
        self.invalidate_completion();
        self.refresh_suggestion();
    }

    pub fn move_home(&mut self) {
        self.cursor = 0;
        self.invalidate_completion();
        self.refresh_suggestion();
    }

    pub fn move_end(&mut self) {
        self.cursor = self.char_count();
        self.invalidate_completion();
        self.refresh_suggestion();
    }

    pub fn history_previous(&mut self) {
        if self.history.is_empty() {
            return;
        }
        self.history_pos = match self.history_pos {
            None => {
                self.draft = self.buffer.clone();
                Some(self.history.len() - 1)
            }
            Some(0) => return,
            Some(pos) => Some(pos - 1),
        };
        self.load_at(self.history_pos.unwrap());
    }

    pub fn history_next(&mut self) {
        let pos = match self.history_pos {
            None => return,
            Some(pos) => pos,
        };
        if pos + 1 < self.history.len() {
            self.history_pos = Some(pos + 1);
            self.load_at(pos + 1);
        } else {
            self.history_pos = None;
            self.buffer = self.draft.clone();
            self.cursor = self.char_count();
            self.view_start = 0;
            self.invalidate_completion();
            self.refresh_suggestion();
        }
    }

    fn load_at(&mut self, pos: usize) {
        self.buffer = self.history[pos].clone();
        self.cursor = self.char_count();
        self.view_start = 0;
        self.invalidate_completion();
        self.refresh_suggestion();
    }

    /// Hand the composed line to the caller, record it in history
    /// (skipping empty and repeated entries) and reset the editor.
    pub fn take_submitted_line(&mut self) -> String {
        let line = self.buffer.clone();
        if !line.is_empty() && self.history.last() != Some(&line) {
            self.history.push(line.clone());
            if self.history.len() > MAX_HISTORY {
                self.history.remove(0);
            }
        }
        self.buffer.clear();
        self.cursor = 0;
        self.view_start = 0;
        self.history_pos = None;
        self.draft.clear();
        self.preedit = None;
        self.invalidate_completion();
        self.refresh_suggestion();
        self.caret_blink_start = Instant::now();
        line
    }

    #[inline]
    pub fn is_locked(&self) -> bool {
        self.locked
    }

    pub fn render(
        &mut self,
        sugarloaf: &mut Sugarloaf,
        dimensions: (f32, f32, f32),
        border_width: f32,
        border_radius: f32,
    ) {
        if !self.enabled || self.height_px <= 0.0 || self.font_size <= 0.0 {
            // Immediate mode: not drawing == not visible.
            return;
        }

        let (window_width, window_height, scale_factor) = dimensions;
        let logical_width = window_width / scale_factor;
        let logical_height = window_height / scale_factor;

        // Framed panel floating inside the reserved band, horizontally
        // centered at the configured width. The center stays fully
        // transparent — the window background (or image) shows through
        // and the bar's own paint is only the thick translucent
        // rounded border.
        let (panel_x, panel_w) = self.panel_geometry(logical_width);
        let panel_y = logical_height - self.height_px + PANEL_INSET_Y;
        let panel_h = (self.height_px - PANEL_INSET_Y * 2.0).max(0.0);

        let border = if self.locked {
            LOCKED_BORDER_COLOR
        } else {
            BORDER_COLOR
        };
        crate::renderer::draw_rounded_frame(
            sugarloaf,
            panel_x,
            panel_y,
            panel_w,
            panel_h,
            border_width,
            border_radius,
            border,
            DEPTH_BG,
            ORDER,
        );

        let font_size = self.font_size;
        let text_y = panel_y + (panel_h - font_size) / 2.0;
        let content_x = panel_x + border_width + TEXT_PADDING_X;
        let content_right = panel_x + panel_w - border_width - TEXT_PADDING_X;

        // Locked: only the hint that keys go to the terminal.
        if self.locked {
            let hint_opts = DrawOpts {
                font_size,
                color: color_u8(DIM_TEXT_COLOR),
                ..DrawOpts::default()
            };
            let max_hint_width = (content_right - content_x).max(0.0);
            let hint =
                crate::renderer::elide_tail(LOCKED_HINT, max_hint_width * 0.5, |s| {
                    sugarloaf.text_mut().measure(s, &hint_opts)
                });
            let hint_width = sugarloaf.text_mut().measure(&hint, &hint_opts);
            sugarloaf.text_mut().draw(
                content_right - hint_width,
                text_y,
                &hint,
                &hint_opts,
            );
            return;
        }

        // Tab-completion candidates row above the main line: dim
        // entries with the cycled one in full colour, which is always
        // included (the row starts far enough back to fit it).
        if let Some(state) = &self.completion {
            if !state.candidates.is_empty() {
                let cand_font = font_size * 0.5;
                let row_y = text_y - cand_font - 6.0;
                if row_y >= panel_y + border_width + 4.0 {
                    let max_row_width = (content_right - content_x).max(0.0);
                    let mut measure_candidate = |text: &str, color: [f32; 4]| -> f32 {
                        let opts = DrawOpts {
                            font_size: cand_font,
                            color: color_u8(color),
                            ..DrawOpts::default()
                        };
                        sugarloaf.text_mut().measure(text, &opts)
                    };
                    let widths: Vec<f32> = state
                        .candidates
                        .iter()
                        .map(|c| measure_candidate(&c.display, TEXT_COLOR))
                        .collect();
                    // Walk back from the highlighted candidate while
                    // the older ones still fit alongside it.
                    let mut start = state.index.min(widths.len() - 1);
                    let mut total = widths[start];
                    while start > 0
                        && total + CANDIDATE_GAP + widths[start - 1] <= max_row_width
                    {
                        total += CANDIDATE_GAP + widths[start - 1];
                        start -= 1;
                    }
                    let mut x = content_x;
                    for (i, candidate) in state.candidates.iter().enumerate().skip(start)
                    {
                        let color = if i == state.index {
                            TEXT_COLOR
                        } else {
                            DIM_TEXT_COLOR
                        };
                        let opts = DrawOpts {
                            font_size: cand_font,
                            color: color_u8(color),
                            ..DrawOpts::default()
                        };
                        let width = widths[i];
                        if x + width > content_right {
                            break;
                        }
                        sugarloaf
                            .text_mut()
                            .draw(x, row_y, &candidate.display, &opts);
                        x += width + CANDIDATE_GAP;
                    }
                }
            }
        }

        // Prompt symbol.
        let prompt_opts = DrawOpts {
            font_size,
            color: color_u8(PROMPT_COLOR),
            ..DrawOpts::default()
        };
        let prompt_width =
            sugarloaf
                .text_mut()
                .draw(content_x, text_y, PROMPT_SYMBOL, &prompt_opts);
        let text_x = content_x + prompt_width + PROMPT_GAP;

        // Right edge of the text area.
        let max_text_width = (content_right - text_x).max(0.0);

        let opts = DrawOpts {
            font_size,
            color: color_u8(TEXT_COLOR),
            ..DrawOpts::default()
        };
        let preedit_opts = DrawOpts {
            font_size,
            color: color_u8(PREEDIT_TEXT_COLOR),
            ..DrawOpts::default()
        };
        let suggestion_opts = DrawOpts {
            font_size,
            color: color_u8(DIM_TEXT_COLOR),
            ..DrawOpts::default()
        };
        let preedit = self.preedit.clone().unwrap_or_default();
        let preedit_width = if preedit.is_empty() {
            0.0
        } else {
            sugarloaf.text_mut().measure(&preedit, &preedit_opts)
        };
        // Ghost suggestion: mutually exclusive with preedit (it is
        // suppressed while composing).
        let suggestion = if self.preedit.is_none() {
            self.suggestion.clone().unwrap_or_default()
        } else {
            String::new()
        };
        let suggestion_width = if suggestion.is_empty() {
            0.0
        } else {
            sugarloaf.text_mut().measure(&suggestion, &suggestion_opts)
        };

        let chars: Vec<char> = self.buffer.chars().collect();
        // The composition text sits at the caret; keep room for it and
        // for the ghost suffix (they are mutually exclusive).
        let budget = max_text_width
            - preedit_width.min(max_text_width)
            - suggestion_width.min(max_text_width);

        // Advance view_start so the text before the cursor fits
        // (binary search: width shrinks monotonically as start grows).
        let mut fits_before = |start: usize| -> bool {
            let prefix: String = chars[start..self.cursor].iter().collect();
            sugarloaf.text_mut().measure(&prefix, &opts) <= budget
        };
        let (mut lo, mut hi) = (self.view_start.min(self.cursor), self.cursor);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if fits_before(mid) {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        self.view_start = lo;

        // Extend past the cursor while the whole visible run fits.
        let mut fits_through = |end: usize| -> bool {
            let visible: String = chars[self.view_start..end].iter().collect();
            sugarloaf.text_mut().measure(&visible, &opts) <= budget
        };
        let (mut lo, mut hi) = (self.cursor, chars.len());
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            if fits_through(mid) {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        let visible: String = chars[self.view_start..lo].iter().collect();
        let prefix: String = chars[self.view_start..self.cursor].iter().collect();

        let prefix_width = sugarloaf.text_mut().measure(&prefix, &opts);
        sugarloaf.text_mut().draw(text_x, text_y, &visible, &opts);

        // Composition ghost text after the caret position, with an
        // underline the text pass cannot draw by itself.
        let mut caret_x = text_x + prefix_width;
        if !preedit.is_empty() {
            sugarloaf
                .text_mut()
                .draw(caret_x, text_y, &preedit, &preedit_opts);
            sugarloaf.rect(
                None,
                caret_x,
                text_y + font_size,
                preedit_width,
                PREEDIT_UNDERLINE_HEIGHT,
                PREEDIT_TEXT_COLOR,
                DEPTH_ELEMENT,
                ORDER,
            );
            caret_x += preedit_width;
        }

        // Ghost suggestion right after the caret (dim); the caret bar
        // overlaps its first pixels, exactly like zsh-autosuggestions.
        if !suggestion.is_empty() {
            sugarloaf
                .text_mut()
                .draw(caret_x, text_y, &suggestion, &suggestion_opts);
        }
        caret_x = caret_x.min(text_x + budget);

        // Blinking caret; solid while composing.
        let caret_visible = self.preedit.is_some() || {
            let elapsed_ms = self.caret_blink_start.elapsed().as_millis();
            (elapsed_ms / CARET_BLINK_MS).is_multiple_of(2)
        };
        if caret_visible {
            sugarloaf.rect(
                None,
                caret_x,
                text_y,
                CARET_WIDTH,
                font_size,
                TEXT_COLOR,
                DEPTH_ELEMENT,
                ORDER,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_editing() {
        let mut bar = InputBar::new(true);
        bar.insert_text("abc");
        assert_eq!(bar.buffer, "abc");
        assert_eq!(bar.cursor, 3);

        bar.move_left();
        bar.insert_text("X");
        assert_eq!(bar.buffer, "abXc");
        assert_eq!(bar.cursor, 3);

        bar.backspace();
        assert_eq!(bar.buffer, "abc");

        bar.move_home();
        bar.delete_forward();
        assert_eq!(bar.buffer, "bc");
        assert_eq!(bar.cursor, 0);

        bar.move_end();
        bar.delete_word_backwards();
        assert_eq!(bar.buffer, "");
    }

    #[test]
    fn control_characters_are_sanitized() {
        let mut bar = InputBar::new(true);
        bar.insert_text("a\nb\tc\u{1b}");
        assert_eq!(bar.buffer, "a b c");
    }

    #[test]
    fn delete_word_matches_bash_rubout() {
        let mut bar = InputBar::new(true);
        bar.insert_text("foo bar baz");
        bar.delete_word_backwards();
        assert_eq!(bar.buffer, "foo bar");
        bar.delete_word_backwards();
        assert_eq!(bar.buffer, "foo");
        bar.delete_word_backwards();
        assert_eq!(bar.buffer, "");
    }

    #[test]
    fn delete_to_line_start() {
        let mut bar = InputBar::new(true);
        bar.insert_text("hello world");
        bar.move_home();
        bar.move_right();
        bar.move_right();
        bar.delete_to_line_start();
        assert_eq!(bar.buffer, "llo world");
        assert_eq!(bar.cursor, 0);
    }

    #[test]
    fn history_browse_and_fork() {
        let mut bar = InputBar::new(true);
        bar.insert_text("first");
        bar.take_submitted_line();
        bar.insert_text("second");
        bar.take_submitted_line();

        // Browse: older, older, then back down to the draft.
        bar.insert_text("draft");
        bar.history_previous();
        assert_eq!(bar.buffer, "second");
        bar.history_previous();
        assert_eq!(bar.buffer, "first");
        bar.history_previous(); // clamped at the oldest
        assert_eq!(bar.buffer, "first");
        bar.history_next();
        assert_eq!(bar.buffer, "second");
        bar.history_next();
        assert_eq!(bar.buffer, "draft");
        bar.history_next(); // no-op on the live line
        assert_eq!(bar.buffer, "draft");

        // Editing a previewed entry forks to the live line.
        bar.history_previous();
        assert_eq!(bar.buffer, "second");
        bar.insert_text("!");
        assert_eq!(bar.buffer, "second!");
        assert_eq!(bar.history_pos, None);
        bar.history_next(); // live line: no-op
        assert_eq!(bar.buffer, "second!");
    }

    #[test]
    fn submit_resets_and_dedups() {
        let mut bar = InputBar::new(true);
        bar.insert_text("ls -la");
        assert_eq!(bar.take_submitted_line(), "ls -la");
        assert_eq!(bar.buffer, "");
        assert_eq!(bar.cursor, 0);

        bar.insert_text("ls -la");
        bar.take_submitted_line();
        assert_eq!(bar.history.len(), 1, "repeat submits dedup");

        // Empty submits go to the PTY as a bare Enter, not to history.
        assert_eq!(bar.take_submitted_line(), "");
        assert_eq!(bar.history.len(), 1);
    }

    #[test]
    fn history_is_capped() {
        let mut bar = InputBar::new(true);
        for i in 0..(MAX_HISTORY + 20) {
            bar.insert_text(&format!("cmd-{i}"));
            bar.take_submitted_line();
        }
        assert_eq!(bar.history.len(), MAX_HISTORY);
        assert_eq!(bar.history.first().unwrap(), "cmd-20");
        assert_eq!(
            bar.history.last().unwrap(),
            &format!("cmd-{}", MAX_HISTORY + 19)
        );
    }

    #[test]
    fn lock_and_preedit_reset_blink() {
        let mut bar = InputBar::new(true);
        assert!(!bar.is_locked());
        bar.set_locked(true);
        assert!(bar.is_locked());
        bar.set_locked(false);
        assert!(!bar.is_locked());

        bar.set_preedit(Some("中".to_string()));
        assert!(bar.has_preedit());
        bar.set_preedit(None);
        assert!(!bar.has_preedit());
    }

    #[test]
    fn height_follows_enabled_and_metrics() {
        let mut bar = InputBar::new(false);
        bar.set_metrics(18.0, 21.5);
        assert_eq!(bar.height(), 0.0, "disabled reserves nothing");

        bar.set_enabled(true);
        // Six terminal lines tall, twice the terminal font size.
        assert_eq!(bar.height(), 21.5 * INPUT_BAR_LINES);
        assert_eq!(bar.font_size, 18.0 * INPUT_FONT_SCALE);

        bar.set_metrics(24.0, 26.0);
        assert_eq!(bar.height(), 26.0 * INPUT_BAR_LINES);
        assert_eq!(bar.font_size, 24.0 * INPUT_FONT_SCALE);
    }

    #[test]
    fn configured_height_overrides_the_lines_default() {
        let mut bar = InputBar::new(true);
        bar.set_metrics(18.0, 21.5);

        bar.set_size(SizeSpec::Percent(100.0), Some(SizeSpec::Px(120.0)));
        assert!(bar.refresh_height(1000.0));
        assert_eq!(bar.height(), 120.0);
        assert!(!bar.refresh_height(999.0), "px height ignores the window");

        bar.set_size(SizeSpec::Percent(100.0), Some(SizeSpec::Percent(25.0)));
        assert!(bar.refresh_height(800.0));
        assert_eq!(bar.height(), 200.0);

        // The floor keeps a stray percent from erasing the bar.
        bar.set_size(SizeSpec::Percent(100.0), Some(SizeSpec::Px(4.0)));
        assert!(bar.refresh_height(800.0));
        assert_eq!(bar.height(), MIN_BAR_HEIGHT);

        // Unset falls back to six terminal lines.
        bar.set_size(SizeSpec::Percent(100.0), None);
        assert!(bar.refresh_height(800.0));
        assert_eq!(bar.height(), 21.5 * INPUT_BAR_LINES);
    }

    #[test]
    fn panel_width_is_centered() {
        let mut bar = InputBar::new(true);

        // Full width (default): spans the whole band.
        let (x, w) = bar.panel_geometry(1000.0);
        assert_eq!(x, PANEL_INSET_X);
        assert_eq!(w, 1000.0 - PANEL_INSET_X * 2.0);

        // 60% of the window, centered.
        bar.set_size(SizeSpec::Percent(60.0), None);
        let (x, w) = bar.panel_geometry(1000.0);
        assert_eq!(w, 600.0);
        assert_eq!(x, (1000.0 - 600.0) / 2.0);

        // Pixels, centered.
        bar.set_size(SizeSpec::Px(400.0), None);
        let (x, w) = bar.panel_geometry(1000.0);
        assert_eq!(w, 400.0);
        assert_eq!(x, 300.0);

        // Oversized configs clamp to the band.
        bar.set_size(SizeSpec::Px(4000.0), None);
        let (x, w) = bar.panel_geometry(1000.0);
        assert_eq!(x, PANEL_INSET_X);
        assert_eq!(w, 1000.0 - PANEL_INSET_X * 2.0);
    }

    // ---- persistent history & ghost suggestions ----

    fn bar_with_history(entries: &[&str]) -> InputBar {
        let mut bar = InputBar::new(true);
        bar.set_history(entries.iter().map(|s| s.to_string()).collect());
        bar
    }

    #[test]
    fn loaded_history_browses_with_draft_fork() {
        let mut bar = bar_with_history(&["old-first", "second", "third"]);
        bar.insert_text("draft");
        bar.history_previous();
        assert_eq!(bar.buffer, "third");
        bar.history_previous();
        assert_eq!(bar.buffer, "second");
        bar.history_next();
        assert_eq!(bar.buffer, "third");
        bar.history_next();
        assert_eq!(bar.buffer, "draft", "back down to the live line");
    }

    #[test]
    fn suggestion_appears_dismisses_and_accepts() {
        let mut bar = bar_with_history(&["cargo build --release", "cargo test", "ls"]);

        bar.insert_text("cargo ");
        assert_eq!(
            bar.suggestion.as_deref(),
            Some("test"),
            "the most recent match wins"
        );

        // Off end-of-line it disappears, back at the end it returns.
        bar.move_left();
        assert_eq!(bar.suggestion, None);
        bar.move_end();
        assert_eq!(bar.suggestion.as_deref(), Some("test"));

        // → takes the whole suffix; nothing equals the buffer remains.
        assert!(bar.accept_suggestion());
        assert_eq!(bar.buffer, "cargo test");
        assert_eq!(bar.suggestion, None);
        assert_eq!(bar.cursor, bar.char_count());
    }

    #[test]
    fn suggestion_suppressed_while_composing_or_disabled() {
        let mut bar = bar_with_history(&["echo hi"]);
        bar.insert_text("ec");
        assert_eq!(bar.suggestion.as_deref(), Some("ho hi"));

        bar.set_preedit(Some("中".to_string()));
        assert_eq!(bar.suggestion, None, "no ghost during composition");
        bar.set_preedit(None);
        assert_eq!(bar.suggestion.as_deref(), Some("ho hi"));

        bar.set_options(true, false);
        assert_eq!(bar.suggestion, None, "config switch off");
        assert!(bar.records_history());
        bar.set_options(false, true);
        assert!(!bar.records_history());
        assert_eq!(bar.suggestion.as_deref(), Some("ho hi"));
    }

    #[test]
    fn exact_match_produces_no_suggestion() {
        let mut bar = bar_with_history(&["git status"]);
        bar.insert_text("git status");
        assert_eq!(bar.suggestion, None, "buffer already equals the entry");
    }

    #[test]
    fn submit_reports_whether_history_took_the_line() {
        let mut bar = InputBar::new(true);
        bar.insert_text("dup");
        assert_eq!(bar.take_submitted_line(), "dup");
        assert!(bar.last_history_is("dup"));

        // Adjacent duplicate: skipped in memory, and the caller sees
        // the same "last" so the store stays deduped too.
        bar.insert_text("dup");
        assert_eq!(bar.take_submitted_line(), "dup");
        assert!(bar.last_history_is("dup"));

        bar.insert_text("other");
        assert_eq!(bar.take_submitted_line(), "other");
        assert!(!bar.last_history_is("dup"));
        assert!(bar.last_history_is("other"));
    }

    // ---- Tab completion ----

    /// Scoped temp dir for completion fixtures (no tempfile dep).
    struct TempDirGuard(std::path::PathBuf);

    impl TempDirGuard {
        fn new(tag: &str) -> Self {
            static COUNTER: std::sync::atomic::AtomicU32 =
                std::sync::atomic::AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("tap-inputbar-{}-{n}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }

        fn sources(&self, cwd: Option<std::path::PathBuf>) -> crate::completion::Sources {
            crate::completion::Sources {
                path_dirs: vec![self.0.clone()],
                home: None,
                cwd,
            }
        }
    }

    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_exec(dir: &std::path::Path, name: &str) {
        std::fs::write(dir.join(name), "").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                dir.join(name),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
    }

    #[test]
    fn tab_unique_command_gets_a_trailing_space() {
        let dir = TempDirGuard::new("cmd");
        write_exec(dir.path(), "hello");
        let sources = dir.sources(None);

        let mut bar = InputBar::new(true);
        bar.insert_text("hel");
        bar.tab_complete(&sources);
        assert_eq!(bar.buffer, "hello ");
        assert_eq!(bar.cursor, bar.char_count());
        assert!(bar.completion.is_none(), "unique match leaves no cycle");

        // A second Tab on the trailing space: empty word, consumed
        // without action.
        bar.tab_complete(&sources);
        assert_eq!(bar.buffer, "hello ");
    }

    #[test]
    fn tab_cycles_candidates_after_common_prefix() {
        let dir = TempDirGuard::new("cycle");
        write_exec(dir.path(), "foobar");
        write_exec(dir.path(), "foobar2");
        let sources = dir.sources(None);

        let mut bar = InputBar::new(true);
        bar.insert_text("foo");
        bar.tab_complete(&sources);
        assert_eq!(bar.buffer, "foobar", "first Tab completes the LCP");
        assert!(bar.completion.is_some());

        // Further Tabs walk the sorted candidates; the LCP word is a
        // real candidate, so it comes around first.
        bar.tab_complete(&sources);
        assert_eq!(bar.buffer, "foobar");
        bar.tab_complete(&sources);
        assert_eq!(bar.buffer, "foobar2");
        bar.tab_complete(&sources);
        assert_eq!(bar.buffer, "foobar", "wraps around");

        // Any edit invalidates the cycle state.
        bar.backspace();
        assert!(bar.completion.is_none());
    }

    #[test]
    fn tab_completes_paths_against_the_cwd() {
        let dir = TempDirGuard::new("paths");
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src").join("main.rs"), "").unwrap();
        std::fs::write(dir.path().join("readme"), "").unwrap();
        let sources = dir.sources(Some(dir.path().to_path_buf()));

        let mut bar = InputBar::new(true);
        // A bare first word would be command mode; `./` forces path
        // mode. Unique dir: the slash lands in the buffer, no space.
        bar.insert_text("./s");
        bar.tab_complete(&sources);
        assert_eq!(bar.buffer, "./src/");

        // Descend: type the file prefix and complete the file.
        bar.insert_text("m");
        bar.tab_complete(&sources);
        assert_eq!(bar.buffer, "./src/main.rs ");
    }

    #[test]
    fn tab_survives_multibyte_words() {
        let dir = TempDirGuard::new("multibyte");
        std::fs::create_dir(dir.path().join("中文目录")).unwrap();
        std::fs::write(dir.path().join("中文文件"), "").unwrap();
        let sources = dir.sources(Some(dir.path().to_path_buf()));

        let mut bar = InputBar::new(true);
        bar.insert_text("./中");
        bar.tab_complete(&sources);
        assert_eq!(bar.buffer, "./中文", "LCP past the typed char");

        // Cycle: byte-sorted inserts put 文件 before 目录.
        bar.tab_complete(&sources);
        assert_eq!(bar.buffer, "./中文文件");
    }

    #[test]
    fn tab_with_no_matches_is_a_consumed_noop() {
        let dir = TempDirGuard::new("nomatch");
        write_exec(dir.path(), "hello");
        let sources = dir.sources(None);

        let mut bar = InputBar::new(true);
        bar.insert_text("zzz");
        bar.tab_complete(&sources);
        assert_eq!(bar.buffer, "zzz");
        assert!(bar.completion.is_none());

        // First word only: a slash word is path mode even at position 0.
        bar.buffer.clear();
        bar.cursor = 0;
        bar.insert_text("no/suchpath");
        bar.tab_complete(&sources);
        assert_eq!(bar.buffer, "no/suchpath");
    }
}
