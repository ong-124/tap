#ifndef LIBTAP_H
#define LIBTAP_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct tap_engine tap_engine_t;
typedef struct tap_surface tap_surface_t;
typedef struct tap_render_state tap_render_state_t;

typedef size_t tap_surface_id_t;

#define TAP_ACTION_SET_TITLE 0u
#define TAP_ACTION_RING_BELL 1u
#define TAP_ACTION_CURSOR_BLINKING_CHANGE 2u
/* OSC 9;4 progress: data_a is the ConEmu state (0 remove, 1 set, 2 error,
   3 indeterminate, 4 paused), data_b the 0-100 value. */
#define TAP_ACTION_PROGRESS 3u
/* OSC 133: a shell command finished (D after C). data_a is its exit
   status as a signed 32-bit value in two's complement (read it as
   (int32_t)data_a), or TAP_EXIT_STATUS_NONE (INT32_MIN) when the shell
   sent none; data_b is how long it ran, in milliseconds. */
#define TAP_ACTION_COMMAND_FINISHED 4u
#define TAP_EXIT_STATUS_NONE 0x80000000u

/* OSC 7501 program status (see program_status_cb). `event` is REPORT,
   or a lifecycle event after which the host drops records: on
   PROMPT_START (OSC 133 A) drop `working` and `blocked` records (and
   optionally `idle`), keeping `done` and `error`; on RESET (RIS) drop
   every record. When the child process exits (close_surface_cb), the
   spec also drops `working` and `blocked` records. libtap validates
   reports against the spec and keeps no records itself, and sends
   lifecycle events only once the surface has reported status. */
#define TAP_PROGRAM_STATUS_REPORT 0u
#define TAP_PROGRAM_STATUS_PROMPT_START 1u
#define TAP_PROGRAM_STATUS_RESET 2u

#define TAP_PROGRAM_STATE_IDLE 0u
#define TAP_PROGRAM_STATE_WORKING 1u
#define TAP_PROGRAM_STATE_DONE 2u
#define TAP_PROGRAM_STATE_BLOCKED 3u
#define TAP_PROGRAM_STATE_ERROR 4u
/* Remove the record at `id` and everything beneath it; with no `id`,
   every record on the surface. */
#define TAP_PROGRAM_STATE_CLEAR 5u

#define TAP_PROGRAM_KIND_NONE 0u
#define TAP_PROGRAM_KIND_PERMISSION 1u
#define TAP_PROGRAM_KIND_QUESTION 2u
#define TAP_PROGRAM_KIND_AUTH 3u

#define TAP_COLOR_NAMED 0u
#define TAP_COLOR_INDEXED 1u
#define TAP_COLOR_RGB 2u
/* An absent color (value and rgb are zero). Returned by
 * tap_render_state_cell_underline_color for cells without an explicit
 * underline color, and as the fg/bg of a missing cell. */
#define TAP_COLOR_NONE 3u

#define TAP_KEY_CHAR 0u
#define TAP_KEY_ENTER 1u
#define TAP_KEY_TAB 2u
#define TAP_KEY_BACKSPACE 3u
#define TAP_KEY_ESCAPE 4u
#define TAP_KEY_UP 5u
#define TAP_KEY_DOWN 6u
#define TAP_KEY_LEFT 7u
#define TAP_KEY_RIGHT 8u
#define TAP_KEY_HOME 9u
#define TAP_KEY_END 10u
#define TAP_KEY_PAGE_UP 11u
#define TAP_KEY_PAGE_DOWN 12u
#define TAP_KEY_INSERT 13u
#define TAP_KEY_DELETE 14u
#define TAP_KEY_F 15u
/* No key: an event that carries only text, such as an input method commit. */
#define TAP_KEY_NONE 16u
/* Modifier keys as keys; reported only in kitty report-all mode. */
#define TAP_KEY_CAPS_LOCK 17u
#define TAP_KEY_SHIFT_LEFT 18u
#define TAP_KEY_SHIFT_RIGHT 19u
#define TAP_KEY_CONTROL_LEFT 20u
#define TAP_KEY_CONTROL_RIGHT 21u
#define TAP_KEY_ALT_LEFT 22u
#define TAP_KEY_ALT_RIGHT 23u
#define TAP_KEY_SUPER_LEFT 24u
#define TAP_KEY_SUPER_RIGHT 25u

#define TAP_KEY_ACTION_PRESS 0u
#define TAP_KEY_ACTION_REPEAT 1u
#define TAP_KEY_ACTION_RELEASE 2u

#define TAP_SELECTION_SIMPLE 0u
#define TAP_SELECTION_WORD 1u
#define TAP_SELECTION_LINE 2u
#define TAP_SELECTION_BLOCK 3u

/* tap_render_state_cursor_shape: DECSCUSR shape, or HIDDEN when the program
   hid the cursor (DECTCEM) or the view is scrolled into history. */
#define TAP_CURSOR_BLOCK 0u
#define TAP_CURSOR_UNDERLINE 1u
#define TAP_CURSOR_BEAM 2u
#define TAP_CURSOR_HIDDEN 3u

#define TAP_MOD_SHIFT (1u << 0)
#define TAP_MOD_CTRL (1u << 1)
#define TAP_MOD_ALT (1u << 2)
#define TAP_MOD_SUPER (1u << 3)

typedef struct {
  uint32_t tag;
  /* Valid only for the duration of the callback. */
  const char *title;
  const char *subtitle;
  /* Numeric payload; meaning depends on tag (see TAP_ACTION_*). */
  uint32_t data_a;
  uint32_t data_b;
} tap_action_s;

/* Strings are UTF-8, NULL when absent, and valid only for the duration
   of the callback. Each report replaces its record wholesale. */
typedef struct {
  uint32_t event;    /* TAP_PROGRAM_STATUS_*; fields below for REPORT only */
  uint32_t state;    /* TAP_PROGRAM_STATE_* */
  uint32_t kind;     /* TAP_PROGRAM_KIND_*; non-NONE only when BLOCKED */
  int32_t progress;  /* 0-100, or -1 when absent (indeterminate) */
  const char *id;    /* slash-separated record path; NULL = root record */
  const char *app;   /* stable program id, e.g. "cargo", "claude-code" */
  const char *msg;   /* one human-readable line */
  const char *title; /* short label for the record */
} tap_program_status_s;

typedef struct {
  void *userdata;
  /* May fire on libtap's PTY IO thread, or synchronously inside any
     tap_surface_* call that changes what is shown. Callbacks must only
     flag or schedule work; calling back into tap_* from inside a callback
     is undefined behavior. wakeup_cb may not be repeated for later output
     until the next tap_render_state_update: clear a "needs render" flag
     before that call, not after, or a wakeup between the two is lost. */
  void (*wakeup_cb)(void *userdata, tap_surface_id_t surface);
  void (*action_cb)(void *userdata, tap_surface_id_t surface, tap_action_s action);
  void (*clipboard_write_cb)(void *userdata, tap_surface_id_t surface, uint8_t kind,
                             const char *utf8);
  void (*close_surface_cb)(void *userdata, tap_surface_id_t surface);
  /* OSC 7501. NULL to ignore program status: the terminal then also
     leaves the support query (OSC 7501 ; ?) unanswered. */
  void (*program_status_cb)(void *userdata, tap_surface_id_t surface,
                            const tap_program_status_s *status);
} tap_runtime_config_s;

typedef struct {
  /* NULL means $SHELL, falling back to /bin/sh. */
  const char *shell;
  const char *working_dir;
  uint16_t cols;
  uint16_t rows;
  uint16_t pixel_width;
  uint16_t pixel_height;
  size_t scrollback;
  /* Arguments for the program, after argv[0]. NULL/0 means none. These reach
   * the child through execvp as separate argv entries, so a host with a
   * command to run can spawn it instead of typing it into the terminal:
   * nothing here is ever read by a shell's line editor. */
  const char *const *args;
  size_t args_len;
} tap_surface_config_s;

typedef struct {
  /* Original form: TAP_COLOR_NAMED / _INDEXED / _RGB. `value` holds the
     named-color id or palette index for the first two. Kind
     TAP_COLOR_NONE means no color at all: value and r/g/b are zero and
     nothing should be drawn from them. */
  uint8_t kind;
  uint16_t value;
  /* The resolved RGB for every kind except TAP_COLOR_NONE, so a CPU
     renderer can read r/g/b directly without owning a palette. */
  uint8_t r;
  uint8_t g;
  uint8_t b;
} tap_color_s;

typedef struct {
  uint32_t codepoint;
  tap_color_s fg;
  tap_color_s bg;
  uint16_t style_flags;
} tap_cell_s;

/* Bit assignments for tap_cell_s.style_flags. Bits 6-10 are the
 * underline kind; at most one is set. At most one blink bit is set.
 * Bits 14-15 are the TAP_CELL_* markers, not styles.
 *
 * tap_cell_s.fg/bg are exported pre-swap: when TAP_STYLE_INVERSE is
 * set the host swaps them when drawing, as tap's own renderer does. */
#define TAP_STYLE_INVERSE (1u << 0)
#define TAP_STYLE_BOLD (1u << 1)
#define TAP_STYLE_ITALIC (1u << 2)
#define TAP_STYLE_DIM (1u << 3)
#define TAP_STYLE_HIDDEN (1u << 4)
#define TAP_STYLE_STRIKEOUT (1u << 5)
#define TAP_STYLE_UNDERLINE (1u << 6)
#define TAP_STYLE_DOUBLE_UNDERLINE (1u << 7)
#define TAP_STYLE_UNDERCURL (1u << 8)
#define TAP_STYLE_DOTTED_UNDERLINE (1u << 9)
#define TAP_STYLE_DASHED_UNDERLINE (1u << 10)
#define TAP_STYLE_SLOW_BLINK (1u << 11)
#define TAP_STYLE_RAPID_BLINK (1u << 12)

typedef struct {
  uint8_t r;
  uint8_t g;
  uint8_t b;
} tap_rgb_s;

/* A color scheme, limited to what the engine itself resolves: the 16 ANSI
 * slots plus the named defaults. Selection/cursor rendering colors stay
 * host-side; `cursor` here covers cells that reference the cursor color. */
typedef struct {
  /* ANSI 0-7 normal, 8-15 bright. */
  tap_rgb_s ansi[16];
  tap_rgb_s foreground;
  tap_rgb_s background;
  tap_rgb_s cursor;
} tap_colors_s;

typedef struct {
  uint16_t line;
  uint16_t column;
} tap_cursor_s;

typedef struct {
  bool active;
  uint16_t start_line;
  uint16_t start_col;
  uint16_t end_line;
  uint16_t end_col;
  bool is_block;
} tap_selection_s;

/* A key event as the platform delivered it.
 *
 * The host reports what happened and libtap decides what the terminal
 * receives, because that depends on state the host does not track:
 * application cursor mode, the kitty keyboard flags, and modifyOtherKeys.
 */
typedef struct {
  /* TAP_KEY_ACTION_*. Releases are dropped unless the program running in the
   * terminal asked for key event types. */
  uint32_t action;
  /* TAP_KEY_*, naming the key with no shift applied: shift+a is
   * TAP_KEY_CHAR with codepoint 'a', and the "A" belongs in text. */
  uint32_t tag;
  uint32_t codepoint;
  /* 1 to 12 when tag is TAP_KEY_F. */
  uint8_t function_key;
  /* Every modifier held, as TAP_MOD_*. */
  uint8_t mods;
  /* Modifiers the platform already spent producing text, so they are not
   * encoded twice. Zero when the platform cannot say. */
  uint8_t consumed_mods;
  /* Whether an input method owns the key right now. Nothing is encoded; the
   * text arrives when composition commits. */
  bool composing;
  /* What the platform produced for this key, UTF-8, after any dead key or
   * input method composition. May be NULL. */
  const char *text;
  size_t text_len;
} tap_key_event_s;

/* Hosts on Linux should ignore SIGPIPE: internal wake pipes written from
 * signal handlers expect EPIPE. On macOS/BSD the sockets opt out per-fd. */
tap_engine_t *tap_engine_new(const tap_runtime_config_s *config);
void tap_engine_free(tap_engine_t *engine);

/* Replace the palette used to resolve named/indexed cell colors, process-wide.
 * NULL restores Tap's default theme. Dim variants derive from the new palette.
 * The host owns redraw: repaint every surface after this (cells re-resolve on
 * the next tap_render_state_cell read; no re-snapshot needed). */
void tap_set_colors(const tap_colors_s *colors);

tap_surface_t *tap_surface_new(tap_engine_t *engine,
                               const tap_surface_config_s *config);
void tap_surface_free(tap_surface_t *surface);
tap_surface_id_t tap_surface_id(const tap_surface_t *surface);
/* Pid of the spawned program, for identity and diagnostics only. Do not
 * signal it on teardown: tap_surface_free already hangs up the process
 * group and an internal reader escalates SIGHUP -> SIGKILL and reaps, so
 * a host-side killpg() races that escalation and can hit a recycled pid.
 * On Windows it is the conpty child's process id. 0 when the surface has
 * no PTY or the pid was unavailable. */
uint32_t tap_surface_child_pid(const tap_surface_t *surface);
/* Deliver SIGHUP to the child's process group through its lifecycle
 * guard: a no-op after the child was reaped, so it never signals a
 * recycled pid or races the internal escalation. Returns true when
 * delivery succeeded (unix with a PTY only). */
bool tap_surface_hangup_child(const tap_surface_t *surface);

/* Input entry points are callable from any thread. */
void tap_surface_text(tap_surface_t *surface, const char *bytes, size_t len);
/* Paste `bytes` (UTF-8), bracketed when the program has enabled mode 2004. */
void tap_surface_paste(tap_surface_t *surface, const char *bytes, size_t len);
/* Terminal modes for input decisions the host makes on its own: bit 0 mouse
 * reporting, bit 1 application cursor keys, bit 2 alternate screen, bit 3
 * bracketed paste. */
uint32_t tap_surface_mode_bits(const tap_surface_t *surface);
/* Returns true when the event was consumed and encoded to the PTY. */
bool tap_surface_key(tap_surface_t *surface, const tap_key_event_s *event);
/* Whether the surface has keyboard focus. A change is reported to a program
 * that asked for focus events (DECSET 1004) as CSI I / CSI O, and a program
 * that enables them is told the current state at once; nothing else is
 * sent, and a repeated value is ignored. A surface starts unfocused. Report
 * the host's own tab and split switches too: a surface the user is not
 * looking at is unfocused to its program. Unlike input, this never scrolls
 * the view or clears the selection. */
void tap_surface_set_focus(tap_surface_t *surface, bool focused);
/* Whether alt acts as meta, prefixing with ESC, instead of letting the text
 * the platform produced through. Defaults to true, as terminals do: it is the
 * difference between alt+d deleting a word and inserting a character. */
void tap_surface_set_alt_is_meta(tap_surface_t *surface, bool enabled);
/* Grapheme cluster processing (DEC private mode 2027) as the default for
 * cell layout. On by default; embedders whose renderers assume legacy
 * wcwidth layout can turn it off. A program's DECSET/DECRST still wins at
 * runtime, and RIS restores this configured default. */
void tap_surface_set_grapheme_clustering(tap_surface_t *surface, bool enabled);
void tap_surface_resize(tap_surface_t *surface, uint16_t cols, uint16_t rows,
                        uint16_t pixel_width, uint16_t pixel_height);
void tap_surface_scroll(tap_surface_t *surface, int32_t delta_lines);
/* A wheel scroll, dispatched by libtap: a mouse report when the program
   asked for mouse events, cursor keys on the alternate screen with
   alternate-scroll on, otherwise the scrollback view. `lines` is positive
   for scrolling up; `col`/`row` are the cell under the pointer. Returns
   true when the program consumed it. */
bool tap_surface_scroll_wheel(tap_surface_t *surface, int32_t lines,
                              uint16_t col, uint16_t row, uint8_t mods);

/* Report a mouse button press/release to the program when it asked for mouse
   events (DEC 1000/1002/1003, SGR/UTF8/X10). `button` is 0=left, 1=middle,
   2=right; `mods` uses the same bits as tap_surface_scroll_wheel. Returns
   true when a report reached the program (the host should then skip its own
   selection); false when nothing grabs the mouse, or shift bypasses to a
   local selection. */
bool tap_surface_mouse_button(tap_surface_t *surface, uint16_t col, uint16_t row,
                              uint8_t button, bool pressed, uint8_t mods);

/* Report pointer motion for button-event (1002, a button held) / any-event
   (1003, bare motion) modes. `button` is 0/1/2 for the button held during a
   drag, or 3 for none. Returns true when a report was emitted. */
bool tap_surface_mouse_motion(tap_surface_t *surface, uint16_t col, uint16_t row,
                              uint8_t button, uint8_t mods);

/* `side_right` is true when the pointer sits in the right half of the cell.
   It decides whether that cell falls inside the selection, so a drag can
   reach the cells at both ends. */
void tap_surface_selection_begin(tap_surface_t *surface, int32_t viewport_line,
                                 uint16_t col, uint8_t kind, bool side_right);
void tap_surface_selection_update(tap_surface_t *surface, int32_t viewport_line,
                                  uint16_t col, bool side_right);
void tap_surface_selection_clear(tap_surface_t *surface);
/* Returns NULL when nothing is selected. Free with tap_text_free. */
char *tap_surface_selection_text(const tap_surface_t *surface);

/* Session persistence: current working directory (OSC 7), NULL if unknown.
 * Free with tap_text_free. */
char *tap_surface_working_dir(const tap_surface_t *surface);
/* Foreground process name (the running program). Free with tap_text_free. */
char *tap_surface_foreground_process_name(const tap_surface_t *surface);
/* Inject bytes into the terminal DISPLAY (not the PTY input), for replaying
 * persisted scrollback on restore. */
void tap_surface_inject_output(tap_surface_t *surface, const char *bytes,
                               size_t len);
/* Whole buffer (scrollback + screen) as UTF-8 text, for persist/replay.
 * Free with tap_text_free. */
char *tap_surface_dump(const tap_surface_t *surface);

void tap_text_free(char *text);

/* Render-state calls must all come from one thread. */
tap_render_state_t *tap_render_state_new(const tap_surface_t *surface);
void tap_render_state_free(tap_render_state_t *state);
void tap_render_state_update(tap_render_state_t *state);
uint16_t tap_render_state_lines(const tap_render_state_t *state);
uint16_t tap_render_state_columns(const tap_render_state_t *state);
bool tap_render_state_row_dirty(const tap_render_state_t *state, uint16_t line);
void tap_render_state_reset_dirty(tap_render_state_t *state);
/* Set in tap_cell_s.style_flags when the cell carries attached cluster
 * codepoints (combining marks, or a DEC-2027 grapheme cluster) beyond
 * `codepoint`. Fetch the full text with tap_render_state_cell_cluster
 * and draw that instead of the base char. Style flags proper occupy
 * bits 0-12 (TAP_STYLE_*); this is bit 15. */
#define TAP_CELL_HAS_CLUSTER (1u << 15)

/* Write the full text of a cell (base codepoint plus attached cluster
 * codepoints) as UTF-32 into `out` (capacity `cap` code units).
 * Returns the total codepoint count, which may exceed `cap` (call
 * again with a larger buffer); 0 for plain cells. */
size_t tap_render_state_cell_cluster(const tap_render_state_t *state,
                                     uint16_t line, uint16_t column,
                                     uint32_t *out, size_t cap);

/* Measure the first grapheme cluster in a UTF-32 buffer: returns the
 * number of codepoints the cluster spans and writes its terminal cell
 * width to `out_width` when non-null (2 for wide, 1 for narrow, 0 for
 * bare zero-width marks); 0 codepoints for a null or empty buffer. Same segmentation and width rules as printing
 * under grapheme clustering (DEC mode 2027). The buffer must contain
 * a complete first cluster or the logical end of the text. Values
 * that are not Unicode scalars measure as one single-width codepoint
 * when first and terminate the cluster when later. */
size_t tap_cluster_width(const uint32_t *codepoints, size_t len,
                         uint8_t *out_width);

/* The cell at (line, column). A NULL state or out-of-range query returns
 * a blank cell whose fg/bg have kind TAP_COLOR_NONE, so a miss is never
 * mistaken for a real black cell; real cells always carry a drawable
 * fg/bg kind. */
tap_cell_s tap_render_state_cell(const tap_render_state_t *state, uint16_t line,
                                 uint16_t column);
/* Set in tap_cell_s.style_flags when the cell carries an explicit SGR 58
 * underline color; fetch it with tap_render_state_cell_underline_color.
 * Without this bit, draw the underline in the color used for the glyph:
 * the fallback tap's own renderer applies. This is bit 14. */
#define TAP_CELL_HAS_UNDERLINE_COLOR (1u << 14)
/* The cell's explicit SGR 58 underline color, or kind TAP_COLOR_NONE
 * when the cell has none (also for a NULL state, an out-of-range cell,
 * or an internal error); see TAP_CELL_HAS_UNDERLINE_COLOR. */
tap_color_s tap_render_state_cell_underline_color(const tap_render_state_t *state,
                                                  uint16_t line, uint16_t column);
tap_cursor_s tap_render_state_cursor(const tap_render_state_t *state);
/* TAP_CURSOR_*; see the defines. */
uint8_t tap_render_state_cursor_shape(const tap_render_state_t *state);

/* This frame's dynamic (OSC 10/11/12) default colors. `which`: 0 =
 * foreground, 1 = background, 2 = cursor. Writes the resolved RGB into
 * `out` and returns true when the program has set that color via OSC;
 * returns false and leaves `out` untouched when unset (including after an
 * OSC 110/111/112 reset), so the host falls back to its own scheme. */
bool tap_render_state_dynamic_color(const tap_render_state_t *state,
                                    uint8_t which, tap_rgb_s *out);

size_t tap_render_state_display_offset(const tap_render_state_t *state);
bool tap_render_state_alt_screen(const tap_render_state_t *state);

const uint8_t *tap_symbols_nerd_font(size_t *len);

/* A kitty image placement resolved to pixels. x/y are relative to the
 * grid origin (add your padding); src_* is the normalized source
 * rectangle (origin + size) to crop from the image. */
typedef struct {
  uint32_t image_id;
  int32_t z_index;
  float x;
  float y;
  float width;
  float height;
  float src_x;
  float src_y;
  float src_w;
  float src_h;
} tap_kitty_placement_s;

size_t tap_render_state_kitty_count(const tap_render_state_t *state);
bool tap_render_state_kitty_placement(const tap_render_state_t *state,
                                      size_t index, float cell_width,
                                      float cell_height,
                                      tap_kitty_placement_s *out);
bool tap_render_state_kitty_image_info(const tap_render_state_t *state,
                                       uint32_t image_id, uint32_t *out_width,
                                       uint32_t *out_height,
                                       uint64_t *out_stamp);
size_t tap_render_state_kitty_image_rgba(const tap_render_state_t *state,
                                         uint32_t image_id, uint8_t *buf,
                                         size_t cap);

/* Image protocols, for tap_image_placement_s.protocol and the
 * tap_render_state_image_* lookups. Ids are only unique within one. */
#define TAP_IMAGE_PROTOCOL_KITTY 0u
/* Sixel and iTerm2 inline images, which share one store. */
#define TAP_IMAGE_PROTOCOL_INLINE 1u

/* Image layers, for tap_image_placement_s.layer. Draw in this order:
 * images below the background, cell backgrounds, images below the text,
 * text, images above the text; within a layer by z-index. */
#define TAP_IMAGE_LAYER_BELOW_BACKGROUND 0u
#define TAP_IMAGE_LAYER_BELOW_TEXT 1u
#define TAP_IMAGE_LAYER_ABOVE_TEXT 2u

/* One rectangle of one image, of any protocol, resolved to viewport
 * pixels. x/y are relative to the grid origin (add your padding); src_*
 * is the normalized source rectangle (origin + size) to crop from the
 * image. Cache textures by (protocol, image_id, image_stamp): the stamp
 * changes whenever the image's pixels do, and is never 0 while they are
 * available, so 0 can stand for "no texture yet". */
typedef struct {
  uint64_t image_id;
  uint64_t image_stamp;
  int32_t z_index;
  uint8_t protocol;
  uint8_t layer;
  float x;
  float y;
  float width;
  float height;
  float src_x;
  float src_y;
  float src_w;
  float src_h;
} tap_image_placement_s;

/* Every image placement in the last snapshot, visible or not, in draw
 * order (by z-index, lowest first; sixel/iTerm2 under kitty at the same
 * z). The kitty-only calls above are a subset of this list: draw from
 * one or the other, never both, or kitty images are drawn twice. Later
 * versions may add protocols; skip a placement whose protocol you don't
 * know. */
size_t tap_render_state_image_count(const tap_render_state_t *state);
/* Changes when an update changes the placement list or moves it on
 * screen; compare for equality to skip rebuilding a draw list. */
uint64_t
tap_render_state_image_placements_generation(const tap_render_state_t *state);
bool tap_render_state_image_placement(const tap_render_state_t *state,
                                      size_t index, float cell_width,
                                      float cell_height,
                                      tap_image_placement_s *out);
bool tap_render_state_image_info(const tap_render_state_t *state,
                                 uint8_t protocol, uint64_t image_id,
                                 uint32_t *out_width, uint32_t *out_height,
                                 uint64_t *out_stamp);
size_t tap_render_state_image_rgba(const tap_render_state_t *state,
                                   uint8_t protocol, uint64_t image_id,
                                   uint8_t *buf, size_t cap);

typedef struct {
  double x;
  double y;
  double width;
  double height;
} tap_glyph_box_s;

bool tap_nerd_constrain(uint32_t codepoint, tap_glyph_box_s glyph,
                        double cell_width, double cell_height,
                        double icon_height_single, uint8_t constraint_width,
                        tap_glyph_box_s *out);
tap_selection_s tap_render_state_selection(const tap_render_state_t *state);

#ifdef __cplusplus
}
#endif

#endif
