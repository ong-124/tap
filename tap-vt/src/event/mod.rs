pub mod sync;

use crate::ansi::graphics::UpdateQueues;
use crate::clipboard::ClipboardType;
use crate::config::colors::ColorRgb;
use crate::crosswords::grid::Scroll;
use crate::crosswords::pos::{Direction, Pos};
use crate::crosswords::search::{Match, RegexSearch};
use crate::error::TapError;
use std::borrow::Cow;
use std::collections::VecDeque;
use std::fmt::Debug;
use std::fmt::Formatter;
use std::sync::Arc;
#[cfg(feature = "tap-window")]
use tap_window::event::Event as TapWindowEvent;

#[cfg(feature = "tap-window")]
use tap_window::event_loop::EventLoopProxy;

/// The core's own window identifier. `tap-vt` never aliases this to a
/// windowing crate's type: it is always a plain id, so the terminal core
/// stays independent of `tap-window`. With the `tap-window` feature,
/// `From` conversions bridge to/from `tap_window::window::WindowId` at the
/// frontend boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WindowId(u64);

impl From<u64> for WindowId {
    fn from(id: u64) -> Self {
        WindowId(id)
    }
}

impl From<WindowId> for u64 {
    fn from(id: WindowId) -> Self {
        id.0
    }
}

#[cfg(feature = "tap-window")]
impl From<WindowId> for tap_window::window::WindowId {
    fn from(id: WindowId) -> Self {
        tap_window::window::WindowId::from(id.0)
    }
}

#[cfg(feature = "tap-window")]
impl From<tap_window::window::WindowId> for WindowId {
    fn from(id: tap_window::window::WindowId) -> Self {
        WindowId(u64::from(id))
    }
}

/// Terminal viewport size, in cells and in pixels.
///
/// Owned by the core so the event model does not name the PTY layer's type
/// for four integers; the PTY driver converts at its own boundary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WindowSize {
    pub rows: u16,
    pub cols: u16,
    pub width: u16,
    pub height: u16,
}

#[cfg(feature = "pty")]
impl From<WindowSize> for teletypewriter::WinsizeBuilder {
    fn from(size: WindowSize) -> Self {
        Self {
            rows: size.rows,
            cols: size.cols,
            width: size.width,
            height: size.height,
        }
    }
}

#[derive(Debug, Clone)]
pub enum TapEventType {
    Tap(TapEvent),
    Frame,
    // Message(Message),
}

#[derive(Debug)]
pub enum Msg {
    /// Data that should be written to the PTY.
    Input(Cow<'static, [u8]>),

    /// The owner's answer to a query the parser could not answer itself
    /// (see `Crosswords::ui_query_sent`), or a reply the terminal routed
    /// through the owner behind such a query (a `PtyWrite` event),
    /// written like `Input`. Once it is queued the writer thread tells
    /// the terminal one more of those is settled, so once the owner has
    /// passed back everything the replies take the direct path again.
    /// Empty when the owner declined to answer.
    Reply(Cow<'static, [u8]>),

    #[allow(dead_code)]
    Shutdown,

    Resize(WindowSize),
}

#[derive(Debug, Eq, PartialEq)]
pub enum ClickState {
    None,
    Click,
    DoubleClick,
    TripleClick,
}

/// Terminal damage hint — coarse signal for the renderer's update path.
/// The actual per-row decision lives on the snapshot's `Row::dirty`
/// (post-`snapshot_visible`); this enum just gates `update` itself
/// (skip vs incremental vs full rebuild). Variants:
/// - `Noop` — no terminal-side change worth rendering for
/// - `Full` — global state changed (resize, palette, mode flip),
///   force a full rebuild even if no individual row is dirty
/// - `Partial` — at least one row's content changed; the snapshot's
///   per-row dirty bits identify which rows
/// - `CursorOnly` — cursor moved/blinked, no cell content changed
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TerminalDamage {
    /// Nothing changed — skip rendering entirely
    #[default]
    Noop,
    /// The entire terminal needs to be redrawn
    Full,
    /// At least one row changed; consult per-row dirty bits
    Partial,
    /// Only the cursor position has changed
    CursorOnly,
}

#[derive(Clone)]
pub enum TapEvent {
    PrepareRender(u64),
    PrepareRenderOnRoute(u64, usize),
    PrepareUpdateConfig,
    /// New terminal content available.
    Render,
    /// A render asked for by a change made outside the parser on one
    /// route, a viewport scroll or a selection; what the parser changes
    /// is announced by the PTY thread's `TerminalDamaged` instead.
    RenderRoute(usize),
    /// Terminal content changed — lightweight notification (no damage payload).
    /// Damage stays in the terminal; renderer extracts it when it locks.
    TerminalDamaged(usize),
    /// Graphics update available from terminal.
    UpdateGraphics {
        route_id: usize,
        queues: UpdateQueues,
    },
    /// A pane's Glyph Protocol registry just became live (first
    /// `register` after session start, or first register following
    /// a clear-all). Frontend installs it into the font library so
    /// subsequent renders consult it. Fires at most once per
    /// (route_id × registry-arc) pair; the registry is Arc-shared,
    /// so further `register`/`clear` mutations made through the
    /// existing handle are visible without re-firing.
    #[cfg(feature = "graphics")]
    GlyphProtocolInstalled {
        route_id: usize,
        registry: tap_graphics::glyph::glyph_registry::GlyphRegistry,
    },
    /// A `q` (query) request arrived from the PTY in `route_id`. The
    /// frontend computes the four-state status — System and/or
    /// Glossary coverage — by consulting both `FontLibrary` (system
    /// fonts) and the per-route glyph registry, then writes the
    /// formatted reply back to the same pane's PTY. Asynchronous
    /// because the dispatcher (in tap-backend) doesn't have access
    /// to the FontLibrary; the frontend does.
    GlyphProtocolQuery {
        route_id: usize,
        cp: u32,
    },
    /// A `q;grapheme=` request (spec §5.3) arrived from the PTY in
    /// `route_id`. `one_cell` is the terminal's half of the answer:
    /// whether the cluster, printed at the cursor in the current mode,
    /// lands in a single cell. The frontend adds the font half, by
    /// resolving and shaping the cluster the way the grid draws it,
    /// and passes the reply back as the answer to a query only it can
    /// answer, which lowers the fence the query raised. Only sent
    /// when the session opted in with `set_glyph_grapheme_query`.
    GlyphProtocolGraphemeQuery {
        route_id: usize,
        cps: Vec<char>,
        one_cell: bool,
    },
    Paste,
    Copy(String),
    UpdateFontSize(u8),
    Scroll(Scroll),
    ToggleFullScreen,
    ToggleAppearanceTheme,
    Minimize(bool),
    Hide,
    HideOtherApplications,
    UpdateConfig,
    CreateWindow,
    ToggleQuake,
    CloseWindow,
    CreateNativeTab(Option<String>),
    CreateConfigEditor,
    SelectNativeTabByIndex(usize),
    SelectNativeTabLast,
    SelectNativeTabNext,
    SelectNativeTabPrev,

    ReportToAssistant(TapError),

    /// Grid has changed possibly requiring a mouse cursor shape change.
    MouseCursorDirty,

    /// Terminal title change from the PTY identified by `route_id`.
    Title(usize, String),

    /// Working directory change (OSC 7) from the PTY identified by
    /// `route_id`. Payload-less: the handler re-reads the terminal's
    /// stored directory, which the emitter already committed.
    CurrentDirectoryChanged(usize),

    /// Ask the event loop to refresh the native titlebar from the
    /// currently displayed pane. Payload-less on purpose: the handler
    /// re-reads the displayed title at handling time, so a queued poke
    /// can never overwrite a newer title with a stale snapshot, and
    /// redundant pokes dedupe at the sink.
    SyncWindowTitle,

    /// Window title change.
    TitleWithSubtitle(String, String),

    /// Reset to the default window title.
    ResetTitle,

    /// Request to store a text string in the clipboard.
    ClipboardStore(ClipboardType, String),

    /// Request to write the contents of the clipboard to the PTY.
    ///
    /// `route_id` identifies the panel that emitted the request so
    /// the bytes land on the originating PTY rather than whichever
    /// panel happens to be focused. The attached function is a
    /// formatter which transforms the clipboard content into the
    /// expected escape-sequence form.
    ClipboardLoad(
        usize,
        ClipboardType,
        Arc<dyn Fn(&str) -> String + Sync + Send + 'static>,
    ),

    /// Request to write the RGB value of a color to the PTY.
    ///
    /// `route_id` identifies the panel that emitted the request so
    /// the reply lands on the originating PTY. The attached function
    /// is a formatter which transforms the RGB color into the
    /// expected escape-sequence form.
    ColorRequest(
        usize,
        usize,
        Arc<dyn Fn(ColorRgb) -> String + Sync + Send + 'static>,
    ),

    /// Write some text to the PTY identified by `route_id`. Routing
    /// by panel (rather than the focused context) is required so
    /// CSI / OSC reply bytes land on the shell that asked for them
    /// even if the user focuses a different split mid-flight.
    PtyWrite(usize, String),

    /// The program on `route_id` just enabled focus events (DECSET 1004)
    /// and is owed the current state, `focused` at the time. A host that
    /// also writes its own focus reports answers with
    /// `Crosswords::focus_report()` read under the terminal lock and
    /// written on the same path as those reports, so the two can never
    /// reach the program out of order; one that handles events
    /// synchronously under the lock can use `focused` as is.
    FocusReport(usize, bool),

    /// Request to write the text area size to the PTY of `route_id`.
    TextAreaSizeRequest(
        usize,
        Arc<dyn Fn(WindowSize) -> String + Sync + Send + 'static>,
    ),

    /// Cursor blinking state has changed.
    CursorBlinkingChange,

    CursorBlinkingChangeOnRoute(usize),

    /// Progress bar report from OSC 9;4 sequence
    ProgressReport(ProgressReport),

    /// OSC 7501 program status, or a lifecycle event that affects it.
    ProgramStatus(ProgramStatusEvent),

    /// Terminal bell ring, from the PTY identified by `route_id`.
    Bell(usize),

    /// OSC 133 `D` after a `C` on the PTY identified by `route_id`: a
    /// shell command finished. `exit_status` is `None` when the shell
    /// sent none; `duration` is measured from the `C` that started the
    /// command's output.
    CommandFinished {
        route_id: usize,
        exit_status: Option<i32>,
        duration: crate::time::Duration,
    },

    /// Desktop notification from OSC 9 or OSC 777.
    DesktopNotification {
        title: String,
        body: String,
    },

    /// Shutdown request.
    Exit,

    /// Quit request.
    Quit,

    /// Leave current terminal.
    CloseTerminal(usize),

    /// The PTY's child process exited, with the raw wait status when the
    /// platform makes it available (interpret with
    /// `std::process::ExitStatus::from_raw` / `ExitStatusExt`).
    ChildExited(usize, Option<i32>),

    BlinkCursor(u64, usize),

    /// Selection scroll tick — auto-scroll while dragging outside viewport.
    SelectionScrollTick,

    /// Update terminal screen colors.
    ///
    /// The first usize is the route_id, the second is the color index to change.
    /// Color index: 0 for foreground, 1 for background, 2 for cursor color.
    ColorChange(usize, usize, Option<ColorRgb>),

    // No operation
    Noop,
}

impl Debug for TapEvent {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            TapEvent::ClipboardStore(ty, text) => {
                write!(f, "ClipboardStore({ty:?}, {text})")
            }
            TapEvent::ClipboardLoad(route_id, ty, _) => {
                write!(f, "ClipboardLoad(route={route_id}, {ty:?})")
            }
            TapEvent::TextAreaSizeRequest(route_id, _) => {
                write!(f, "TextAreaSizeRequest(route={route_id})")
            }
            TapEvent::ColorRequest(route_id, index, _) => {
                write!(f, "ColorRequest(route={route_id}, idx={index})")
            }
            TapEvent::PtyWrite(route_id, text) => {
                write!(f, "PtyWrite(route={route_id}, {text})")
            }
            TapEvent::FocusReport(route_id, focused) => {
                write!(f, "FocusReport(route={route_id}, focused={focused})")
            }
            TapEvent::Title(route_id, title) => {
                write!(f, "Title route {route_id} ({title})")
            }
            TapEvent::CurrentDirectoryChanged(route_id) => {
                write!(f, "CurrentDirectoryChanged route {route_id}")
            }
            TapEvent::SyncWindowTitle => write!(f, "SyncWindowTitle"),
            TapEvent::TitleWithSubtitle(title, subtitle) => {
                write!(f, "TitleWithSubtitle({title}, {subtitle})")
            }
            TapEvent::Minimize(cond) => write!(f, "Minimize({cond})"),
            TapEvent::Hide => write!(f, "Hide)"),
            TapEvent::HideOtherApplications => write!(f, "HideOtherApplications)"),
            TapEvent::CursorBlinkingChange => write!(f, "CursorBlinkingChange"),
            TapEvent::CursorBlinkingChangeOnRoute(route_id) => {
                write!(f, "CursorBlinkingChangeOnRoute {route_id}")
            }
            TapEvent::ProgressReport(report) => {
                write!(f, "ProgressReport({:?})", report)
            }
            TapEvent::ProgramStatus(event) => write!(f, "ProgramStatus({event:?})"),
            TapEvent::MouseCursorDirty => write!(f, "MouseCursorDirty"),
            TapEvent::ResetTitle => write!(f, "ResetTitle"),
            TapEvent::PrepareUpdateConfig => write!(f, "PrepareUpdateConfig"),
            TapEvent::PrepareRender(millis) => write!(f, "PrepareRender({millis})"),
            TapEvent::PrepareRenderOnRoute(millis, route) => {
                write!(f, "PrepareRender({millis} on route {route})")
            }
            TapEvent::Render => write!(f, "Render"),
            TapEvent::RenderRoute(route) => write!(f, "Render route {route}"),
            TapEvent::TerminalDamaged(route_id) => {
                write!(f, "TerminalDamaged route {route_id}")
            }
            #[cfg(feature = "graphics")]
            TapEvent::GlyphProtocolInstalled { route_id, .. } => {
                write!(f, "GlyphProtocolInstalled route {route_id}")
            }
            TapEvent::GlyphProtocolQuery { route_id, cp } => {
                write!(f, "GlyphProtocolQuery route {route_id} cp {cp:#x}")
            }
            TapEvent::GlyphProtocolGraphemeQuery {
                route_id,
                cps,
                one_cell,
            } => {
                write!(
                    f,
                    "GlyphProtocolGraphemeQuery route {route_id} cps {cps:?} one_cell {one_cell}"
                )
            }
            TapEvent::Scroll(scroll) => write!(f, "Scroll {scroll:?}"),
            TapEvent::Bell(route_id) => write!(f, "Bell route {route_id}"),
            TapEvent::CommandFinished {
                route_id,
                exit_status,
                duration,
            } => write!(
                f,
                "CommandFinished route {route_id} status {exit_status:?} after {duration:?}"
            ),
            TapEvent::DesktopNotification { title, body } => {
                write!(f, "DesktopNotification({title}, {body})")
            }
            TapEvent::Exit => write!(f, "Exit"),
            TapEvent::Quit => write!(f, "Quit"),
            TapEvent::CloseTerminal(route) => write!(f, "CloseTerminal {route}"),
            TapEvent::ChildExited(route, status) => {
                write!(f, "ChildExited(route={route}, status={status:?})")
            }
            TapEvent::CreateWindow => write!(f, "CreateWindow"),
            TapEvent::ToggleQuake => write!(f, "ToggleQuake"),
            TapEvent::CloseWindow => write!(f, "CloseWindow"),
            TapEvent::CreateNativeTab(_) => write!(f, "CreateNativeTab"),
            TapEvent::SelectNativeTabByIndex(tab_index) => {
                write!(f, "SelectNativeTabByIndex({tab_index})")
            }
            TapEvent::SelectNativeTabLast => write!(f, "SelectNativeTabLast"),
            TapEvent::SelectNativeTabNext => write!(f, "SelectNativeTabNext"),
            TapEvent::SelectNativeTabPrev => write!(f, "SelectNativeTabPrev"),
            TapEvent::CreateConfigEditor => write!(f, "CreateConfigEditor"),
            TapEvent::UpdateConfig => write!(f, "ReloadConfiguration"),
            TapEvent::ReportToAssistant(error_report) => {
                write!(f, "ReportToAssistant({})", error_report.report)
            }
            TapEvent::ToggleFullScreen => write!(f, "FullScreen"),
            TapEvent::ToggleAppearanceTheme => write!(f, "ToggleAppearanceTheme"),
            TapEvent::BlinkCursor(timeout, route_id) => {
                write!(f, "BlinkCursor {timeout} {route_id}")
            }
            TapEvent::SelectionScrollTick => write!(f, "SelectionScrollTick"),
            TapEvent::Noop => write!(f, "Noop"),
            TapEvent::Copy(_) => write!(f, "Copy"),
            TapEvent::Paste => write!(f, "Paste"),
            TapEvent::UpdateFontSize(action) => write!(f, "UpdateFontSize({action:?})"),
            TapEvent::UpdateGraphics { route_id, .. } => {
                write!(f, "UpdateGraphics({route_id})")
            }
            TapEvent::ColorChange(route_id, color, rgb) => {
                write!(f, "ColorChange({route_id}, {color:?}, {rgb:?})")
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct EventPayload {
    /// Event payload.
    pub payload: TapEventType,
    pub window_id: WindowId,
}

impl EventPayload {
    pub fn new(payload: TapEventType, window_id: WindowId) -> Self {
        Self { payload, window_id }
    }
}

#[cfg(feature = "tap-window")]
impl From<EventPayload> for TapWindowEvent<EventPayload> {
    fn from(event: EventPayload) -> Self {
        TapWindowEvent::UserEvent(event)
    }
}

pub trait OnResize {
    fn on_resize(&mut self, window_size: WindowSize);
}

/// Event Loop for notifying the renderer about terminal events.
pub trait EventListener {
    fn send_event(&self, _event: TapEvent, _id: WindowId) {}

    fn send_event_with_high_priority(&self, _event: TapEvent, _id: WindowId) {}

    fn send_redraw(&self, _id: WindowId) {}

    fn send_global_event(&self, _event: TapEvent) {}
}

#[derive(Clone)]
pub struct VoidListener;

impl From<TapEvent> for TapEventType {
    fn from(tap_event: TapEvent) -> Self {
        Self::Tap(tap_event)
    }
}

impl EventListener for VoidListener {}

#[derive(Debug, Clone)]
#[cfg(feature = "tap-window")]
pub struct EventProxy {
    proxy: EventLoopProxy<EventPayload>,
}

#[cfg(feature = "tap-window")]
impl EventProxy {
    pub fn new(proxy: EventLoopProxy<EventPayload>) -> Self {
        Self { proxy }
    }

    pub fn send_event(&self, event: TapEventType, id: WindowId) {
        let _ = self.proxy.send_event(EventPayload::new(event, id));
    }
}

#[cfg(feature = "tap-window")]
impl EventListener for EventProxy {
    fn send_event(&self, event: TapEvent, id: WindowId) {
        let _ = self.proxy.send_event(EventPayload::new(event.into(), id));
    }
}

/// Regex search state.
pub struct SearchState {
    /// Search direction.
    pub direction: Direction,

    /// Current position in the search history.
    pub history_index: Option<usize>,

    /// Change in display offset since the beginning of the search.
    pub display_offset_delta: i32,

    /// Search origin in viewport coordinates relative to original display offset.
    pub origin: Pos,

    /// Focused match during active search.
    pub focused_match: Option<Match>,

    /// Search regex and history.
    ///
    /// During an active search, the first element is the user's current input.
    ///
    /// While going through history, the [`SearchState::history_index`] will point to the element
    /// in history which is currently being previewed.
    pub history: VecDeque<String>,

    /// Compiled search automatons.
    pub dfas: Option<RegexSearch>,
}

impl SearchState {
    /// Search regex text if a search is active.
    pub fn regex(&self) -> Option<&String> {
        self.history_index.and_then(|index| self.history.get(index))
    }

    /// Direction of the search from the search origin.
    pub fn direction(&self) -> Direction {
        self.direction
    }

    /// Focused match during vi-less search.
    pub fn focused_match(&self) -> Option<&Match> {
        self.focused_match.as_ref()
    }

    /// Clear the focused match.
    pub fn clear_focused_match(&mut self) {
        self.focused_match = None;
    }

    /// Active search dfas.
    pub fn dfas_mut(&mut self) -> Option<&mut RegexSearch> {
        self.dfas.as_mut()
    }

    /// Active search dfas.
    pub fn dfas(&self) -> Option<&RegexSearch> {
        self.dfas.as_ref()
    }

    /// Search regex text if a search is active.
    pub fn regex_mut(&mut self) -> Option<&mut String> {
        self.history_index
            .and_then(move |index| self.history.get_mut(index))
    }
}

impl Default for SearchState {
    fn default() -> Self {
        Self {
            direction: Direction::Right,
            display_offset_delta: Default::default(),
            focused_match: Default::default(),
            history_index: Default::default(),
            history: Default::default(),
            origin: Default::default(),
            dfas: Default::default(),
        }
    }
}

/// Progress bar state for OSC 9;4 ConEmu/Windows Terminal progress reporting
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressState {
    /// Remove/hide the progress bar (state 0)
    Remove,
    /// Set progress with a specific percentage (state 1)
    Set,
    /// Show error state (state 2)
    Error,
    /// Indeterminate/pulsing progress (state 3)
    Indeterminate,
    /// Paused progress (state 4)
    Pause,
}

/// Progress report from OSC 9;4 sequence
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProgressReport {
    /// The progress bar state
    pub state: ProgressState,
    /// Optional progress percentage (0-100), only used with Set, Error, and Pause states
    pub progress: Option<u8>,
}

/// Body of the reply to the OSC 7501 support query (`OSC 7501 ; ? ST`):
/// `?` plus the states and kinds this terminal accepts, so a program can
/// fall back when it wants one from a later revision of the spec.
/// `clear` is left out because it is not a state.
pub const PROGRAM_STATUS_QUERY_REPLY: &str =
    "?:states=idle,working,done,blocked,error:kinds=permission,question,auth";

/// `state` of an OSC 7501 program status report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgramState {
    /// At rest, waiting for the user's next instruction.
    Idle,
    Working,
    Done,
    /// Waiting on the user; `ProgramStatus::kind` says for what.
    Blocked,
    Error,
    /// Remove the addressed record and everything beneath it (every
    /// record on the terminal when the report has no `id`).
    Clear,
}

/// Why a `blocked` program is waiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockedKind {
    Permission,
    Question,
    Auth,
}

/// One validated OSC 7501 report. Each report replaces its record
/// wholesale, so absent fields mean "not set", not "unchanged".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramStatus {
    pub state: ProgramState,
    /// Only ever set on `Blocked`.
    pub kind: Option<BlockedKind>,
    /// Stable program identifier, e.g. `cargo` or `claude-code`.
    pub app: Option<String>,
    /// One human-readable line (decoded from base64).
    pub msg: Option<String>,
    /// Short label for the record (decoded from base64).
    pub title: Option<String>,
    /// 0-100; only on `Working` or `Blocked`, absent means indeterminate.
    pub progress: Option<u8>,
    /// Slash-separated record path; `None` addresses the root record.
    pub id: Option<String>,
}

/// What the terminal tells its embedder about program status. Records
/// live in the embedder; the lifecycle events tell it when the spec
/// requires dropping some of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgramStatusEvent {
    Report(ProgramStatus),
    /// OSC 133 `A`: a new shell prompt began, so `working` and `blocked`
    /// records must be dropped (`idle` may be; `done`/`error` survive).
    PromptStart,
    /// Full reset (RIS): every record is removed.
    Reset,
}
