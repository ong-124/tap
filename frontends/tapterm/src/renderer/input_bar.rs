// Copyright (c) 2023-present, Raphael Amorim.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

use std::time::{Duration, Instant};
use tap_backend::config::input_bar::SizeSpec;
use tap_backend::sugarloaf::text::DrawOpts;
use tap_backend::sugarloaf::Sugarloaf;

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

/// How long a dynamic completion answer stays trusted. Cycling
/// candidates or re-Tabbing within the window never re-queries; past
/// it the next Tab asks the tool again (pods come and go).
const DYNAMIC_CACHE_TTL: Duration = Duration::from_secs(10);

const PROMPT_SYMBOL: &str = "\u{276F}";
/// Prompt prefix while a broadcast is armed.
const BROADCAST_MARKER: &str = ">> ";
const LOCKED_HINT: &str = "keys pass through";
/// One bullet per character while the line is masked (password mode).
const MASK_CHAR: char = '•';

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
/// Keyboard input lands here unless the focused pane runs a full-screen
/// program (alt screen), which locks the bar and passes keys straight
/// to the terminal. A full line is written to the PTY on Enter; lines
/// typed while the pty has echo off (password prompts) are masked and
/// kept out of history.
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
    /// Whether the live line was masked when history browsing stashed
    /// it, so coming back down restores the mask.
    draft_masked: bool,
    /// Password mode: draw bullets, suppress suggestions, skip history.
    /// Sticky once a line turns masked (an echo recovery mid-line must
    /// never re-reveal typed characters); resets with the line.
    masked: bool,
    preedit: Option<String>,
    /// Ghost suggestion (zsh-autosuggestions style): the suffix of the
    /// most recent history entry matching the buffer as a prefix,
    /// rendered dim after the caret and accepted with → / End.
    suggestion: Option<String>,
    /// The full history entry the ghost came from, so an edit that
    /// still extends toward it just trims the tail instead of
    /// rescanning (Warp editor/view/mod.rs:7725-7774).
    suggestion_line: Option<String>,
    /// Per-entry cwd parallel to `history` (empty = unknown), used to
    /// rank same-directory history first for the ghost suggestion.
    history_cwds: Vec<String>,
    /// cwd of the focused pane, snapshotted by the screen layer on
    /// submit / Tab; `None` until then (cwd-agnostic ranking).
    focus_cwd: Option<String>,
    /// Live Tab-completion cycle state; kept only while the word under
    /// the cursor is still the one this state last produced.
    completion: Option<CompletionState>,
    /// `[input-bar] dynamic-completion` — Cobra `__complete` queries,
    /// ssh hosts, remote lookups.
    dynamic: bool,
    /// `[input-bar] command-aliases` — alias → Cobra tool mapping.
    aliases: std::collections::HashMap<String, String>,
    /// ssh destination of the session running in the focused pane,
    /// snapshotted on each Tab from the completion sources.
    session_ssh_host: Option<String>,
    /// The session's shell-reported (OSC 7) cwd, snapshotted alongside
    /// the host — the anchor for remote path completion.
    session_remote_cwd: Option<String>,
    /// In-flight dynamic query: the request key plus the channel its
    /// worker thread will answer on. Dropped on lock, config reload
    /// or when a newer request replaces it.
    pending: Option<PendingDynamic>,
    /// Recent dynamic answers, so cycling candidates or re-Tabbing
    /// doesn't re-query the cluster (TTL-bounded for freshness).
    dynamic_cache: std::collections::HashMap<
        String,
        (Instant, Vec<crate::completion::Candidate>, bool),
    >,
    /// `[input-bar] record-history` — persist submitted lines.
    record_history: bool,
    /// `[input-bar] suggestions` — ghost-text suggestions on/off.
    suggestions: bool,
    /// Mirror of the screen's broadcast toggle, drawn as a `>>`
    /// prompt marker while on.
    broadcast: bool,
    caret_blink_start: Instant,
}

/// A dynamic query awaiting its worker thread's answer.
struct PendingDynamic {
    key: String,
    rx: std::sync::mpsc::Receiver<crate::completion::DynamicResult>,
}

/// Tab-completion cycle: the word region it owns, the word currently
/// in the buffer, the candidate list and which candidate sits in the
/// buffer right now (`candidates.len()` = sentinel for "only the LCP
/// is shown, the menu starts on the next Tab").
struct CompletionState {
    /// Char index where the completed word begins.
    word_start: usize,
    /// Word text currently in the buffer (LCP or last candidate).
    current: String,
    candidates: Vec<crate::completion::Candidate>,
    /// Candidate currently applied; the render highlights this one.
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
            draft_masked: false,
            masked: false,
            preedit: None,
            suggestion: None,
            suggestion_line: None,
            history_cwds: Vec::new(),
            focus_cwd: None,
            completion: None,
            dynamic: true,
            aliases: std::collections::HashMap::new(),
            session_ssh_host: None,
            session_remote_cwd: None,
            pending: None,
            dynamic_cache: std::collections::HashMap::new(),
            record_history: true,
            suggestions: true,
            broadcast: false,
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
    /// Only tests call it today; the renderer resolves metrics directly.
    #[allow(dead_code)]
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
                self.suggestion_line = None;
                self.completion = None;
                // An in-flight answer belongs to a prompt that is gone;
                // drop it (the worker finishes harmlessly on its own).
                self.pending = None;
            } else {
                self.caret_blink_start = Instant::now();
            }
        }
    }

    #[inline]
    pub fn has_preedit(&self) -> bool {
        self.preedit.is_some()
    }

    /// Mirror the screen's broadcast toggle for the `>>` prompt marker
    /// (the screen owns the flag and the fan-out; the bar only shows).
    pub fn set_broadcast(&mut self, broadcast: bool) {
        self.broadcast = broadcast;
    }

    /// Turn on password masking (the caller probes the pty's echo
    /// state; echo off = a prompt reading a secret). One-way for the
    /// lifetime of the line: only `clear` / `take_submitted_line` /
    /// leaving for a history entry lift it, so a mid-line echo recovery
    /// can never re-reveal what was typed.
    pub fn set_masked(&mut self, masked: bool) {
        if masked && !self.masked {
            self.masked = true;
            self.suggestion = None;
            self.suggestion_line = None;
            self.completion = None;
            self.caret_blink_start = Instant::now();
        }
    }

    #[inline]
    pub fn is_masked(&self) -> bool {
        self.masked
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
    /// store). Any in-flight browse resets to the live line. The
    /// per-entry cwd the store recorded rides along: the ghost
    /// suggestion prefers entries from the focused pane's directory
    /// (Warp next_command_model.rs:839-864).
    pub fn set_history_entries(&mut self, entries: Vec<crate::input_history::Entry>) {
        self.history_cwds = entries.iter().map(|e| e.cwd.clone()).collect();
        self.reset_history(entries.into_iter().map(|e| e.line).collect());
    }

    fn reset_history(&mut self, history: Vec<String>) {
        self.history = history;
        self.history_pos = None;
        self.draft.clear();
        self.draft_masked = false;
        self.masked = false;
        self.suggestion_line = None;
        self.invalidate_completion();
        self.refresh_suggestion();
    }

    /// Snapshot the focused pane's cwd (screen layer refreshes it on
    /// submit and Tab, both points where the pane's directory is
    /// already being probed).
    pub fn set_focus_cwd(&mut self, cwd: Option<String>) {
        if self.focus_cwd != cwd {
            self.focus_cwd = cwd;
            // Ranking context changed: drop the trim shortcut so the
            // scan re-ranks against the new directory.
            self.suggestion_line = None;
            self.refresh_suggestion();
        }
    }

    /// Stamp the cwd a just-submitted line ran in onto the newest
    /// history entry (the push itself doesn't know it). Called by the
    /// screen layer right after `take_submitted_line`.
    pub fn set_last_history_cwd(&mut self, cwd: &str) {
        if let Some(last) = self.history_cwds.last_mut() {
            *last = cwd.to_string();
        }
    }

    /// `[input-bar] record-history` / `suggestions` / `dynamic-completion`
    /// / `command-aliases`.
    pub fn set_options(
        &mut self,
        record_history: bool,
        suggestions: bool,
        dynamic: bool,
        aliases: std::collections::HashMap<String, String>,
    ) {
        self.record_history = record_history;
        self.suggestions = suggestions;
        if self.dynamic != dynamic || self.aliases != aliases {
            // Routing just changed: an in-flight answer no longer
            // matches what the new configuration would ask for.
            self.pending = None;
            self.session_ssh_host = None;
        }
        self.dynamic = dynamic;
        self.aliases = aliases;
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

    /// Whether a submitted line belongs in history: non-blank and
    /// without a leading space. Shared gate for the in-memory list and
    /// (via the screen layer) the persistent store.
    #[inline]
    pub fn records_history_line(&self, line: &str) -> bool {
        !line.is_empty() && !line.starts_with(' ') && !line.trim().is_empty()
    }

    fn invalidate_completion(&mut self) {
        self.completion = None;
    }

    /// Recompute the ghost suggestion. Only meaningful at end-of-line,
    /// outside composition and history browsing — otherwise `None`.
    ///
    /// Two-step: an edit that still points at the current suggestion's
    /// line just trims the tail (Warp editor/view/mod.rs:7725-7774 —
    /// no rescan, so the ghost never jumps to a different entry while
    /// typing toward it); otherwise a fresh scan prefers entries whose
    /// recorded cwd matches the focused pane's, then everything else,
    /// most recent first in each bucket.
    fn refresh_suggestion(&mut self) {
        let eligible = self.suggestions
            && !self.masked
            && self.preedit.is_none()
            && self.history_pos.is_none()
            && self.cursor == self.char_count()
            && !self.buffer.is_empty();
        if !eligible {
            self.suggestion = None;
            self.suggestion_line = None;
            return;
        }

        // Progressive trim: the buffer is still a strict prefix of the
        // line behind the current ghost — covers appends (tail shrinks)
        // and backspace (tail grows back).
        if let Some(line) = self.suggestion_line.clone() {
            if line.starts_with(self.buffer.as_str()) && line != self.buffer {
                self.suggestion = Some(line[self.buffer.len()..].to_string());
                return;
            }
        }

        // Fresh scan, newest → oldest, same-cwd bucket first. Entries
        // without cwd metadata (plain `set_history`, in-session pushes
        // before the screen stamps them) never claim the priority.
        let buffer = self.buffer.clone();
        let same_cwd = |i: usize| {
            self.focus_cwd
                .as_deref()
                .filter(|focus| !focus.is_empty())
                .zip(self.history_cwds.get(i).map(String::as_str))
                .is_some_and(|(focus, cwd)| focus == cwd && !cwd.is_empty())
        };
        let mut primary: Option<usize> = None;
        let mut fallback: Option<usize> = None;
        for i in (0..self.history.len()).rev() {
            let entry = &self.history[i];
            if entry == &buffer || !entry.starts_with(buffer.as_str()) {
                continue;
            }
            if primary.is_none() && same_cwd(i) {
                primary = Some(i);
                break;
            }
            if fallback.is_none() {
                fallback = Some(i);
            }
        }
        match primary.or(fallback) {
            Some(i) => {
                let entry = self.history[i].clone();
                self.suggestion = Some(entry[self.buffer.len()..].to_string());
                self.suggestion_line = Some(entry);
            }
            None => {
                self.suggestion = None;
                self.suggestion_line = None;
            }
        }
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

    /// Ctrl+→ at end-of-line: accept the ghost's next word, the
    /// remainder stays as the ghost (Warp mod.rs:6006-6076). Returns
    /// whether a suggestion was consumed; the caller falls back to the
    /// plain word-right passthrough otherwise.
    pub fn accept_suggestion_word(&mut self) -> bool {
        let Some(suggestion) = self.suggestion.clone() else {
            return false;
        };
        // First word end of the ghost: skip the leading whitespace run,
        // then take the word itself (whitespace-delimited, matching the
        // rest of the bar's word logic).
        let chars: Vec<char> = suggestion.chars().collect();
        let mut end = 0;
        while end < chars.len() && chars[end].is_whitespace() {
            end += 1;
        }
        while end < chars.len() && !chars[end].is_whitespace() {
            end += 1;
        }
        if end == 0 {
            return false;
        }
        self.fork_from_history();
        let accepted: String = chars[..end].iter().collect();
        self.buffer.push_str(&accepted);
        self.cursor = self.char_count();
        self.view_start = 0;
        self.invalidate_completion();
        let remaining: String = chars[end..].iter().collect();
        if remaining.is_empty() {
            self.suggestion = None;
            self.suggestion_line = None;
        } else {
            // The buffer is still a strict prefix of the suggestion's
            // line — keep the ghost, no rescan.
            self.suggestion = Some(remaining);
        }
        true
    }

    /// Tab: complete the word before the cursor. First press inserts
    /// the common prefix (or the single candidate plus a `/` dir /
    /// space suffix); further presses on an unchanged word cycle the
    /// full candidates. An empty word or no matches consumes the key
    /// without acting.
    ///
    /// Routing decides where candidates come from: the static sources
    /// (PATH commands, pane cwd paths), the ssh host table, or a
    /// dynamic query (`__complete`, compgen) that fills the row when
    /// its worker answers.
    pub fn tab_complete(&mut self, sources: &crate::completion::Sources) {
        if self.preedit.is_some() || self.masked {
            return;
        }
        self.session_ssh_host = sources.session_ssh_host.clone();
        self.session_remote_cwd = sources.session_remote_cwd.clone();

        let (word_start, word, word_idx, argv) = self.token_context();

        // Cycle: same word start and same word text as the live state.
        // `index` is the candidate currently sitting in the buffer, so
        // the highlight always matches what was applied. `current`
        // stores the *unquoted* word — the buffer holds the escaped
        // insert, the tokenizer hands back the unescaped text.
        if let Some(state) = self.completion.take() {
            if state.word_start == word_start
                && state.current == word
                && !state.candidates.is_empty()
            {
                let next = if state.index >= state.candidates.len() {
                    0
                } else {
                    (state.index + 1) % state.candidates.len()
                };
                let insert = state.candidates[next].insert.clone();
                let current = crate::completion::unescape_word(&insert);
                self.apply_word(word_start, &insert);
                self.completion = Some(CompletionState {
                    index: next,
                    current,
                    ..state
                });
                self.refresh_suggestion();
                return;
            }
            // Stale state: dropped, a fresh completion runs below.
        }

        match crate::completion::route(
            &argv,
            word_idx,
            &word,
            word_start,
            &self.aliases,
            sources.session_ssh_host.as_deref(),
            sources.session_remote_cwd.as_deref(),
        ) {
            crate::completion::Route::Dynamic(request) => {
                if self.dynamic {
                    self.start_dynamic(request, word_start, &word);
                } else {
                    // The feature is off: legacy static completion.
                    if word.is_empty() {
                        return;
                    }
                    let candidates = crate::completion::filter_dirs_only(
                        crate::completion::gather(word_start, &word, sources),
                        &argv,
                    );
                    self.apply_candidates_at(candidates, word_start, &word, false);
                }
            }
            crate::completion::Route::SshHosts => {
                if word.is_empty() {
                    return;
                }
                let candidates = crate::completion::ssh_host_candidates(
                    &word,
                    sources.home.as_deref(),
                );
                self.apply_candidates_at(candidates, word_start, &word, false);
            }
            crate::completion::Route::Command | crate::completion::Route::Path => {
                if word.is_empty() {
                    return;
                }
                let candidates = crate::completion::filter_dirs_only(
                    crate::completion::gather(word_start, &word, sources),
                    &argv,
                );
                self.apply_candidates_at(candidates, word_start, &word, false);
            }
            crate::completion::Route::None => {}
        }
    }

    /// Split the buffer before the cursor into shell-lite tokens and
    /// describe the word being completed: its raw char span, its text,
    /// its token index and the argv of complete words before it.
    fn token_context(&self) -> (usize, String, usize, Vec<String>) {
        let prefix: Vec<char> = self.buffer.chars().take(self.cursor).collect();
        let tokens = crate::completion::tokenize(&prefix);
        let at_word_start = prefix.last().is_none_or(|c| c.is_whitespace());
        if at_word_start {
            let argv: Vec<String> = tokens.into_iter().map(|t| t.text).collect();
            (self.cursor, String::new(), argv.len(), argv)
        } else {
            let mut tokens = tokens;
            let last = tokens.pop().expect("a token exists past a word start");
            let word_start = last.start;
            let word = last.text;
            let argv: Vec<String> = tokens.into_iter().map(|t| t.text).collect();
            (word_start, word, argv.len(), argv)
        }
    }

    /// The dynamic request the current buffer state would issue (used
    /// both when spawning and when checking whether a late answer
    /// still matches what is on screen).
    fn current_dynamic_request(&self) -> Option<crate::completion::DynamicRequest> {
        let (word_start, word, word_idx, argv) = self.token_context();
        match crate::completion::route(
            &argv,
            word_idx,
            &word,
            word_start,
            &self.aliases,
            self.session_ssh_host.as_deref(),
            self.session_remote_cwd.as_deref(),
        ) {
            crate::completion::Route::Dynamic(request) => Some(request),
            _ => None,
        }
    }

    /// Answer a dynamic route: serve from cache, else spawn the query
    /// (replacing any older in-flight one) and consume the Tab
    /// silently — the row fills when the answer arrives.
    fn start_dynamic(
        &mut self,
        request: crate::completion::DynamicRequest,
        word_start: usize,
        word: &str,
    ) {
        let key = request.key();

        if let Some(entry) = self.dynamic_cache.get(&key) {
            let (at, candidates, no_space) = entry;
            if at.elapsed() < DYNAMIC_CACHE_TTL {
                self.apply_candidates_at(candidates.clone(), word_start, word, *no_space);
                return;
            }
            self.dynamic_cache.remove(&key);
        }

        // Already in flight with the same key — just wait for it.
        if self.pending.as_ref().is_some_and(|p| p.key == key) {
            return;
        }

        self.pending = Some(PendingDynamic {
            key,
            rx: crate::completion::spawn_dynamic(request),
        });
        self.prune_dynamic_cache();
    }

    /// Non-blocking check on the in-flight query. The result applies
    /// only when the buffer still sits at the exact state that issued
    /// the request — typing meanwhile invalidates the answer.
    pub fn poll_dynamic(&mut self) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        let Ok(result) = pending.rx.try_recv() else {
            self.pending = Some(pending); // still running — keep waiting
            return;
        };
        if self.masked || self.preedit.is_some() {
            return;
        }
        let Some(request) = self.current_dynamic_request() else {
            return;
        };
        if request.key() != pending.key {
            return;
        }
        self.cache_dynamic(&pending.key, &result);
        let (word_start, word, _, _) = self.token_context();
        self.apply_candidates_at(result.candidates, word_start, &word, result.no_space);
    }

    fn cache_dynamic(&mut self, key: &str, result: &crate::completion::DynamicResult) {
        self.dynamic_cache.insert(
            key.to_string(),
            (Instant::now(), result.candidates.clone(), result.no_space),
        );
        self.prune_dynamic_cache();
    }

    /// Expire stale entries; drop everything when the map outgrows a
    /// sane bound (burst queries) — re-querying is cheap relative to
    /// unbounded growth.
    fn prune_dynamic_cache(&mut self) {
        self.dynamic_cache
            .retain(|_, (at, _, _)| at.elapsed() < DYNAMIC_CACHE_TTL);
        if self.dynamic_cache.len() > 128 {
            self.dynamic_cache.clear();
        }
    }

    /// Shared candidate application: single candidate inserts (space
    /// unless dir/no-space), several share the common prefix and open
    /// the cycle menu. The dynamic poll path reuses this.
    ///
    /// Before anything is applied, the candidate must be a superset of
    /// the word being completed (Warp input.rs:13736-13741): a late or
    /// cached dynamic answer that does not extend the typed word is
    /// dropped rather than shortening the user's input. The comparison
    /// folds case, mirroring the case-insensitive candidate match.
    fn apply_candidates_at(
        &mut self,
        candidates: Vec<crate::completion::Candidate>,
        word_start: usize,
        word: &str,
        no_space: bool,
    ) {
        match candidates.len() {
            0 => {}
            1 => {
                let candidate_word =
                    crate::completion::unescape_word(&candidates[0].insert);
                if !crate::completion::starts_with_ignore_case(&candidate_word, word) {
                    return;
                }
                let mut insert = candidates[0].insert.clone();
                if !candidates[0].is_dir && !no_space {
                    insert.push(' ');
                }
                self.apply_word(word_start, &insert);
                self.refresh_suggestion();
            }
            _ => {
                let inserts: Vec<&str> =
                    candidates.iter().map(|c| c.insert.as_str()).collect();
                let prefix = crate::completion::common_prefix(&inserts);
                let prefix_word = crate::completion::unescape_word(&prefix);
                // Guard: every candidate must extend the typed word
                // (case-folded, like the match that produced them). A
                // stale answer containing non-extensions is dropped
                // whole — its LCP would shorten the input, and a cycle
                // menu over it would do the same on the next Tab. The
                // LCP itself cannot stand in: mixed-case matches share
                // no exact-case prefix, yet each still extends the word.
                let extends_word = |candidate: &crate::completion::Candidate| {
                    let candidate_word =
                        crate::completion::unescape_word(&candidate.insert);
                    crate::completion::starts_with_ignore_case(&candidate_word, word)
                };
                if !candidates.iter().all(extends_word) {
                    return;
                }
                let inserted = prefix_word.chars().count() > word.chars().count();
                if inserted {
                    self.apply_word(word_start, &prefix);
                }
                // When the LCP is a full candidate it already sits in
                // the buffer (index 0); otherwise the menu has not
                // started — the sentinel `len` makes the next Tab apply
                // candidates[0], and the row highlights it as upcoming.
                let index = if candidates[0].insert == prefix {
                    0
                } else {
                    candidates.len()
                };
                self.completion = Some(CompletionState {
                    word_start,
                    // Unquoted form: the buffer holds the escaped
                    // insert, the next Tab compares tokenized text.
                    // Without an inserted LCP (e.g. mixed-case
                    // candidates with an empty prefix) the typed word
                    // is what still sits in the buffer.
                    current: if inserted {
                        prefix_word
                    } else {
                        word.to_string()
                    },
                    candidates,
                    index,
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

    /// True when the editable line holds no characters.
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
        self.view_start = 0;
        self.history_pos = None;
        self.draft.clear();
        self.draft_masked = false;
        self.masked = false;
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
                self.draft_masked = self.masked;
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
            // Back to the stashed live line: restore its mask with it.
            self.masked = self.draft_masked;
            self.invalidate_completion();
            self.refresh_suggestion();
        }
    }

    fn load_at(&mut self, pos: usize) {
        self.buffer = self.history[pos].clone();
        self.cursor = self.char_count();
        self.view_start = 0;
        // History entries are public record: never masked.
        self.masked = false;
        self.invalidate_completion();
        self.refresh_suggestion();
    }

    /// Hand the composed line to the caller, record it in history
    /// (skipping empty, repeated, masked and space-prefixed entries)
    /// and reset the editor. Masked lines — passwords — never reach
    /// history, memory or persistent alike; lines with a leading space
    /// or blank lines stay out too (Warp's ignorespace semantics,
    /// shell/mod.rs:182-210 — type ` <cmd>` for an unrecorded run).
    pub fn take_submitted_line(&mut self) -> String {
        let line = self.buffer.clone();
        let secret = self.masked;
        if !secret
            && self.records_history_line(&line)
            && self.history.last() != Some(&line)
        {
            self.history.push(line.clone());
            // cwd unknown here; the screen layer stamps it right after.
            self.history_cwds.push(String::new());
            if self.history.len() > MAX_HISTORY {
                self.history.remove(0);
                self.history_cwds.remove(0);
            }
        }
        self.buffer.clear();
        self.cursor = 0;
        self.view_start = 0;
        self.history_pos = None;
        self.draft.clear();
        self.draft_masked = false;
        self.masked = false;
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

        // A dynamic query may have finished since the last frame; its
        // answer fills the candidates row while the buffer is intact.
        self.poll_dynamic();

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
                    // The candidate in the buffer (sentinel `len` =
                    // LCP only shown → the first candidate is next up).
                    let highlighted = if state.index >= state.candidates.len() {
                        0
                    } else {
                        state.index
                    };
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
                    let mut start = highlighted;
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
                        let color = if i == highlighted {
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

        // Prompt symbol, prefixed with the broadcast marker while a
        // fan-out is armed: one draw call, the marker shares the
        // prompt color and the width math carries over untouched.
        let prompt_opts = DrawOpts {
            font_size,
            color: color_u8(PROMPT_COLOR),
            ..DrawOpts::default()
        };
        let prompt = if self.broadcast {
            format!("{BROADCAST_MARKER}{PROMPT_SYMBOL}")
        } else {
            PROMPT_SYMBOL.to_string()
        };
        let prompt_width =
            sugarloaf
                .text_mut()
                .draw(content_x, text_y, &prompt, &prompt_opts);
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

        let mut chars: Vec<char> = self.buffer.chars().collect();
        // Masked (password) lines draw one bullet per character; the
        // char count is unchanged, so the scroll window and caret math
        // carry over untouched.
        if self.masked {
            chars.fill(MASK_CHAR);
        }
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
        bar.set_history_entries(
            entries
                .iter()
                .map(|line| crate::input_history::Entry {
                    ts: 0,
                    cwd: String::new(),
                    line: line.to_string(),
                    exit: None,
                })
                .collect(),
        );
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

        bar.set_options(true, false, true, std::collections::HashMap::new());
        assert_eq!(bar.suggestion, None, "config switch off");
        assert!(bar.records_history());
        bar.set_options(false, true, true, std::collections::HashMap::new());
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
    fn suggestion_prefers_the_focus_cwd_bucket() {
        let entry = |ts: u64, cwd: &str, line: &str| crate::input_history::Entry {
            ts,
            cwd: cwd.to_string(),
            line: line.to_string(),
            exit: None,
        };
        let mut bar = InputBar::new(true);
        bar.set_history_entries(vec![
            entry(1, "/proj", "make build"),
            entry(2, "/other", "make test"),
        ]);

        // No focus snapshot yet: most recent match wins.
        bar.insert_text("make");
        assert_eq!(bar.suggestion.as_deref(), Some(" test"));

        // Focused on /proj: the older same-directory entry outranks the
        // newer foreign one.
        bar.set_focus_cwd(Some("/proj".to_string()));
        assert_eq!(bar.suggestion.as_deref(), Some(" build"));

        // Entries without cwd metadata never claim the priority.
        bar.set_focus_cwd(None);
        assert_eq!(bar.suggestion.as_deref(), Some(" test"));
    }

    #[test]
    fn suggestion_trims_instead_of_rescanning() {
        let mut bar = bar_with_history(&["cargo test", "cargo ten"]);
        bar.insert_text("car");
        assert_eq!(bar.suggestion.as_deref(), Some("go ten"));

        // Typing toward the suggested line trims the ghost tail without
        // a rescan — a rescan would jump to "cargo test" ("st"), the
        // trim keeps "cargo ten" ("n").
        bar.insert_text("go te");
        assert_eq!(bar.suggestion.as_deref(), Some("n"));

        // Backspacing grows the tail back.
        bar.backspace();
        assert_eq!(bar.suggestion.as_deref(), Some("en"));

        // Diverging from the line falls back to a rescan.
        bar.insert_text("x");
        assert_eq!(bar.suggestion, None);
    }

    #[test]
    fn ctrl_right_accepts_the_suggestion_word_by_word() {
        let mut bar = bar_with_history(&["cargo build --release"]);
        bar.insert_text("cargo");
        assert_eq!(bar.suggestion.as_deref(), Some(" build --release"));

        assert!(bar.accept_suggestion_word());
        assert_eq!(bar.buffer, "cargo build");
        assert_eq!(bar.suggestion.as_deref(), Some(" --release"));

        assert!(bar.accept_suggestion_word());
        assert_eq!(bar.buffer, "cargo build --release");
        assert_eq!(bar.suggestion, None, "the ghost is exhausted");

        // Without a ghost the key falls through to the caller.
        assert!(!bar.accept_suggestion_word());
    }

    #[test]
    fn submitted_lines_get_their_cwd_stamped() {
        let mut bar = bar_with_history(&["ls"]);
        bar.insert_text("make");
        bar.take_submitted_line();
        bar.set_last_history_cwd("/proj");

        bar.set_focus_cwd(Some("/proj".to_string()));
        bar.insert_text("ma");
        assert_eq!(
            bar.suggestion.as_deref(),
            Some("ke"),
            "the stamped entry ranks first in its own directory"
        );
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
                session_ssh_host: None,
                session_remote_cwd: None,
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
    fn tab_path_completion_ignores_case() {
        let dir = TempDirGuard::new("ci-path");
        let cwd = dir.path().join("work");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::write(cwd.join("Notes.md"), "").unwrap();
        let sources = dir.sources(Some(cwd));

        // Any case in the query inserts the real file name.
        let mut bar = InputBar::new(true);
        bar.insert_text("cat NOT");
        bar.tab_complete(&sources);
        assert_eq!(bar.buffer, "cat Notes.md ");
    }

    #[test]
    fn tab_after_cd_lists_directories_only() {
        let dir = TempDirGuard::new("dirs-only");
        let cwd = dir.path().join("work");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::write(cwd.join("dot.txt"), "").unwrap();
        std::fs::create_dir(cwd.join("Documents")).unwrap();
        let sources = dir.sources(Some(cwd));

        // `cd d<Tab>` folds onto the directory — the file never shows
        // up (bash/zsh semantics).
        let mut bar = InputBar::new(true);
        bar.insert_text("cd d");
        bar.tab_complete(&sources);
        assert_eq!(bar.buffer, "cd Documents/");
        assert!(bar.completion.is_none(), "unique match applies outright");

        // The same word after another command still sees both.
        let mut bar = InputBar::new(true);
        bar.insert_text("ls d");
        bar.tab_complete(&sources);
        assert!(bar.completion.is_some(), "ls d<Tab> keeps files and dirs");
        assert_eq!(bar.buffer, "ls d");
    }

    #[test]
    fn tab_mixed_case_candidates_open_menu_without_shortening() {
        let dir = TempDirGuard::new("ci-menu");
        let cwd = dir.path().join("work");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::write(cwd.join("Foo.txt"), "").unwrap();
        std::fs::write(cwd.join("foobar"), "").unwrap();
        let sources = dir.sources(Some(cwd));

        // The LCP is empty ('F' vs 'f'), so the first Tab only opens
        // the cycle menu — never shortens the typed word — and the
        // next applies the exact-case candidate.
        let mut bar = InputBar::new(true);
        bar.insert_text("cat f");
        bar.tab_complete(&sources);
        assert_eq!(bar.buffer, "cat f");
        assert!(bar.completion.is_some());
        bar.tab_complete(&sources);
        assert_eq!(bar.buffer, "cat foobar", "exact-case candidate first");
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

        // The LCP already sits in the buffer, so the next Tab moves
        // straight to the next candidate and then wraps around.
        bar.tab_complete(&sources);
        assert_eq!(bar.buffer, "foobar2");
        bar.tab_complete(&sources);
        assert_eq!(bar.buffer, "foobar", "wraps around");

        // Any edit invalidates the cycle state.
        bar.backspace();
        assert!(bar.completion.is_none());
    }

    #[test]
    fn cycle_highlight_matches_the_buffer_word() {
        // The autossh case: the LCP ("autossh") is itself a candidate,
        // so the fresh highlight must sit on it, not on the next one.
        let dir = TempDirGuard::new("highlight");
        write_exec(dir.path(), "autossh");
        write_exec(dir.path(), "autossh-arv0");
        let sources = dir.sources(None);

        let mut bar = InputBar::new(true);
        bar.insert_text("auto");
        bar.tab_complete(&sources);
        assert_eq!(bar.buffer, "autossh");
        assert_eq!(
            bar.completion.as_ref().unwrap().index,
            0,
            "highlight the candidate actually in the buffer"
        );

        bar.tab_complete(&sources);
        assert_eq!(bar.buffer, "autossh-arv0");
        assert_eq!(
            bar.completion.as_ref().unwrap().index,
            1,
            "highlight follows the applied candidate"
        );

        bar.tab_complete(&sources);
        assert_eq!(bar.buffer, "autossh");
        assert_eq!(bar.completion.as_ref().unwrap().index, 0, "wraps back");
    }

    #[test]
    fn lcp_only_shown_starts_the_menu_at_the_first_candidate() {
        let dir = TempDirGuard::new("lcponly");
        write_exec(dir.path(), "foobar");
        write_exec(dir.path(), "foobaz");
        let sources = dir.sources(None);

        let mut bar = InputBar::new(true);
        bar.insert_text("foo");
        bar.tab_complete(&sources);
        assert_eq!(bar.buffer, "fooba", "LCP is not a full candidate");
        let state = bar.completion.as_ref().unwrap();
        assert_eq!(state.index, state.candidates.len(), "menu not started");

        bar.tab_complete(&sources);
        assert_eq!(bar.buffer, "foobar", "first candidate comes first");
        assert_eq!(bar.completion.as_ref().unwrap().index, 0);

        bar.tab_complete(&sources);
        assert_eq!(bar.buffer, "foobaz");
        assert_eq!(bar.completion.as_ref().unwrap().index, 1);
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
    fn tab_escapes_special_filenames() {
        let dir = TempDirGuard::new("escaping");
        std::fs::write(dir.path().join("my file.txt"), "").unwrap();
        std::fs::create_dir(dir.path().join("new dir")).unwrap();
        let sources = dir.sources(Some(dir.path().to_path_buf()));

        let mut bar = InputBar::new(true);
        // The user types the escape as they go: the tokenizer hands the
        // completer the unescaped word `./my fi`.
        bar.insert_text("./my\\ fi");
        bar.tab_complete(&sources);
        // The insert is shell-escaped: the produced command runs as-is.
        assert_eq!(bar.buffer, "./my\\ file.txt ");

        // Escaped directory candidates keep the trailing slash.
        bar.clear();
        bar.insert_text("./new");
        bar.tab_complete(&sources);
        assert_eq!(bar.buffer, "./new\\ dir/");
    }

    #[test]
    fn tab_cycles_escaped_candidates() {
        let dir = TempDirGuard::new("escaping-cycle");
        std::fs::write(dir.path().join("a file"), "").unwrap();
        std::fs::write(dir.path().join("a file2"), "").unwrap();
        let sources = dir.sources(Some(dir.path().to_path_buf()));

        let mut bar = InputBar::new(true);
        bar.insert_text("./a");
        bar.tab_complete(&sources);
        assert_eq!(bar.buffer, "./a\\ file", "LCP of escaped inserts");
        assert_eq!(bar.completion.as_ref().unwrap().index, 0);
        // The cycle state compares unquoted words, so cycling works
        // across the escaped buffer content.
        bar.tab_complete(&sources);
        assert_eq!(bar.buffer, "./a\\ file2");
        bar.tab_complete(&sources);
        assert_eq!(bar.buffer, "./a\\ file", "wraps around");
    }

    #[test]
    fn stale_candidates_never_shorten_the_typed_word() {
        let mut bar = InputBar::new(true);
        bar.insert_text("kubectl get pod my-app-x");
        // A late dynamic answer whose candidates do not extend the
        // typed word is dropped instead of shrinking the input.
        let candidates = vec![crate::completion::Candidate {
            display: "zzz".to_string(),
            insert: "zzz".to_string(),
            is_dir: false,
        }];
        bar.apply_candidates_at(candidates, 16, "my-app-x", false);
        assert_eq!(bar.buffer, "kubectl get pod my-app-x");

        // Same guard on the multi-candidate LCP: no insert, no menu.
        let candidates = vec![
            crate::completion::Candidate {
                display: "zz".to_string(),
                insert: "zz".to_string(),
                is_dir: false,
            },
            crate::completion::Candidate {
                display: "zzz".to_string(),
                insert: "zzz".to_string(),
                is_dir: false,
            },
        ];
        bar.apply_candidates_at(candidates, 16, "my-app-x", false);
        assert_eq!(bar.buffer, "kubectl get pod my-app-x");
        assert!(bar.completion.is_none());
    }

    #[test]
    fn leading_space_and_blank_lines_stay_out_of_history() {
        let mut bar = InputBar::new(true);
        bar.insert_text("ls -la");
        bar.take_submitted_line();

        bar.insert_text(" secret-cmd");
        assert_eq!(bar.take_submitted_line(), " secret-cmd");
        bar.insert_text("   ");
        bar.take_submitted_line();

        assert_eq!(
            bar.history.len(),
            1,
            "space-prefixed and blank lines never record"
        );
        assert!(!bar.last_history_is(" secret-cmd"));
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

    #[test]
    fn masking_is_sticky_and_skips_history() {
        let mut bar = InputBar::new(true);
        bar.insert_text("ls -la");
        bar.take_submitted_line();

        bar.insert_text("hunter2");
        bar.set_masked(true);
        // A mid-line echo recovery must never unmask the line.
        bar.set_masked(false);
        assert!(bar.is_masked());

        let line = bar.take_submitted_line();
        assert_eq!(line, "hunter2");
        assert!(!bar.is_masked(), "submit resets the mask");
        assert_eq!(
            bar.history.last().map(String::as_str),
            Some("ls -la"),
            "the secret never enters memory history"
        );
        assert!(!bar.last_history_is(&line));
    }

    #[test]
    fn masking_resets_with_the_line() {
        let mut bar = InputBar::new(true);
        bar.insert_text("s3cret");
        bar.set_masked(true);
        bar.clear();
        assert!(!bar.is_masked());

        bar.insert_text("s3cret");
        bar.set_masked(true);
        bar.take_submitted_line();
        assert!(!bar.is_masked());
    }

    #[test]
    fn history_browsing_unmasks_and_the_draft_remasks() {
        let mut bar = InputBar::new(true);
        bar.insert_text("ls -la");
        bar.take_submitted_line();

        bar.insert_text("s3cret");
        bar.set_masked(true);
        bar.history_previous();
        assert_eq!(bar.buffer, "ls -la");
        assert!(!bar.is_masked(), "history entries are public");

        bar.history_next();
        assert_eq!(bar.buffer, "s3cret");
        assert!(bar.is_masked(), "the draft restores its mask");
    }

    #[test]
    fn suggestion_suppressed_while_masked() {
        let mut bar = InputBar::new(true);
        bar.insert_text("ls -la");
        bar.take_submitted_line();

        bar.insert_text("l");
        assert_eq!(bar.suggestion.as_deref(), Some("s -la"));

        bar.set_masked(true);
        assert_eq!(bar.suggestion, None, "no ghost over a secret");
        bar.insert_text("s");
        assert_eq!(bar.suggestion, None);
    }

    // ---- Dynamic completion (`__complete` protocol / ssh) ----

    fn get_pod_request() -> crate::completion::DynamicRequest {
        crate::completion::DynamicRequest::Cobra {
            tool: "kubectl".into(),
            args: vec!["get".into(), "pod".into()],
            word: String::new(),
            host: None,
        }
    }

    /// Pre-load a finished dynamic answer, as if the worker had been
    /// spawned at Tab time and resolved while the user waited.
    fn feed_dynamic(
        bar: &mut InputBar,
        request: crate::completion::DynamicRequest,
        result: crate::completion::DynamicResult,
    ) {
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(result).unwrap();
        bar.pending = Some(PendingDynamic {
            key: request.key(),
            rx,
        });
    }

    fn pods_result(
        candidates: &[&str],
        no_space: bool,
    ) -> crate::completion::DynamicResult {
        crate::completion::DynamicResult {
            candidates: candidates
                .iter()
                .map(|name| crate::completion::Candidate {
                    display: (*name).to_string(),
                    insert: (*name).to_string(),
                    is_dir: false,
                })
                .collect(),
            no_space,
        }
    }

    #[test]
    fn dynamic_answer_opens_the_cycle_menu() {
        let mut bar = InputBar::new(true);
        bar.insert_text("kubectl get pod ");
        feed_dynamic(
            &mut bar,
            get_pod_request(),
            pods_result(&["my-app-1", "my-app-2"], false),
        );

        bar.poll_dynamic();
        assert_eq!(
            bar.buffer, "kubectl get pod my-app-",
            "the LCP lands first, menu opens"
        );
        let state = bar.completion.as_ref().unwrap();
        assert_eq!(state.index, state.candidates.len(), "menu not started yet");

        // Cycling works exactly like static completion from here.
        bar.tab_complete(&empty_sources());
        assert_eq!(bar.buffer, "kubectl get pod my-app-1");
        bar.tab_complete(&empty_sources());
        assert_eq!(bar.buffer, "kubectl get pod my-app-2");
    }

    #[test]
    fn dynamic_single_candidate_space_and_no_space() {
        let mut bar = InputBar::new(true);
        bar.insert_text("kubectl get pod ");
        feed_dynamic(&mut bar, get_pod_request(), pods_result(&["cache"], false));
        bar.poll_dynamic();
        assert_eq!(bar.buffer, "kubectl get pod cache ");

        // `:2` directive — flag values must not gain a trailing space.
        let mut bar = InputBar::new(true);
        bar.insert_text("kubectl get pod ");
        feed_dynamic(&mut bar, get_pod_request(), pods_result(&["cache"], true));
        bar.poll_dynamic();
        assert_eq!(bar.buffer, "kubectl get pod cache");
    }

    #[test]
    fn dynamic_answer_from_a_stale_buffer_is_dropped() {
        let mut bar = InputBar::new(true);
        bar.insert_text("kubectl get pod ");
        feed_dynamic(
            &mut bar,
            get_pod_request(),
            pods_result(&["my-app-1", "my-app-2"], false),
        );

        // The user kept typing after the Tab: the answer no longer
        // matches the live request.
        bar.insert_text("x");
        bar.poll_dynamic();
        assert_eq!(bar.buffer, "kubectl get pod x", "nothing applied");
        assert!(bar.completion.is_none());
    }

    #[test]
    fn dynamic_is_silent_until_the_worker_answers() {
        let mut bar = InputBar::new(true);
        bar.insert_text("kubectl get pod ");
        // No pending seeded, nothing in flight: poll must not act.
        bar.poll_dynamic();
        assert_eq!(bar.buffer, "kubectl get pod ");
        assert!(bar.completion.is_none());
    }

    #[test]
    fn dynamic_suppressed_while_masked() {
        let mut bar = InputBar::new(true);
        bar.insert_text("kubectl get pod ");
        bar.set_masked(true);
        feed_dynamic(&mut bar, get_pod_request(), pods_result(&["cache"], false));
        bar.poll_dynamic();
        assert_eq!(bar.buffer, "kubectl get pod ", "no completion over secrets");
    }

    fn empty_sources() -> crate::completion::Sources {
        crate::completion::Sources {
            path_dirs: Vec::new(),
            home: None,
            cwd: None,
            session_ssh_host: None,
            session_remote_cwd: None,
        }
    }
}
