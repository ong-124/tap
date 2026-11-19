// Copyright (c) 2023-present, Raphael Amorim.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

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

const CARET_WIDTH: f32 = 2.0;
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
    /// Reserved height in logical px (INPUT_BAR_LINES × cell height).
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
    caret_blink_start: Instant,
}

impl Default for InputBar {
    fn default() -> Self {
        Self {
            enabled: false,
            locked: false,
            font_size: 0.0,
            height_px: 0.0,
            buffer: String::new(),
            cursor: 0,
            view_start: 0,
            history: Vec::new(),
            history_pos: None,
            draft: String::new(),
            preedit: None,
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
        self.height_px = cell_height * INPUT_BAR_LINES;
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
        self.set_metrics(font_size, cell.cell_height as f32 / scale);
    }

    /// Update the lock state from the focused pane's zone. Unlocking
    /// restarts the blink so the caret is immediately visible again.
    pub fn set_locked(&mut self, locked: bool) {
        if self.locked != locked {
            self.locked = locked;
            if !locked {
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
        }
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
    }

    pub fn delete_forward(&mut self) {
        if self.cursor >= self.char_count() {
            return;
        }
        self.fork_from_history();
        let start = self.byte_index(self.cursor);
        let end = self.byte_index(self.cursor + 1);
        self.buffer.drain(start..end);
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
    }

    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.char_count());
    }

    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor = self.char_count();
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
        }
    }

    fn load_at(&mut self, pos: usize) {
        self.buffer = self.history[pos].clone();
        self.cursor = self.char_count();
        self.view_start = 0;
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

        // Framed panel floating inside the reserved band. The center
        // stays fully transparent — the window background (or image)
        // shows through and the bar's own paint is only the thick
        // translucent rounded border.
        let panel_x = PANEL_INSET_X;
        let panel_y = logical_height - self.height_px + PANEL_INSET_Y;
        let panel_w = (logical_width - PANEL_INSET_X * 2.0).max(0.0);
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
        let preedit = self.preedit.clone().unwrap_or_default();
        let preedit_width = if preedit.is_empty() {
            0.0
        } else {
            sugarloaf.text_mut().measure(&preedit, &preedit_opts)
        };

        let chars: Vec<char> = self.buffer.chars().collect();
        // The composition text sits at the caret; keep room for it.
        let budget = max_text_width - preedit_width.min(max_text_width);

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
}
