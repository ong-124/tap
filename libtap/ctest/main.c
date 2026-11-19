#include "../include/libtap.h"

#include <stdatomic.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

static atomic_int wakeups = 0;

static void on_wakeup(void *userdata, tap_surface_id_t surface) {
  (void)userdata;
  (void)surface;
  atomic_fetch_add(&wakeups, 1);
}

static int wait_for(tap_render_state_t *state, const char *needle) {
  for (int attempt = 0; attempt < 200; attempt++) {
    tap_render_state_update(state);
    uint16_t lines = tap_render_state_lines(state);
    uint16_t cols = tap_render_state_columns(state);
    for (uint16_t line = 0; line < lines; line++) {
      char text[512] = {0};
      uint16_t limit = cols < 511 ? cols : 511;
      for (uint16_t col = 0; col < limit; col++) {
        tap_cell_s cell = tap_render_state_cell(state, line, col);
        text[col] = (cell.codepoint >= 32 && cell.codepoint < 127)
                        ? (char)cell.codepoint
                        : ' ';
      }
      if (strstr(text, needle)) {
        printf("row %u: %s\n", line, text);
        return 1;
      }
    }
    usleep(25 * 1000);
  }
  return 0;
}

int main(void) {
  tap_runtime_config_s config = {0};
  config.wakeup_cb = on_wakeup;

  tap_engine_t *engine = tap_engine_new(&config);
  if (!engine) {
    fprintf(stderr, "engine creation failed\n");
    return 1;
  }

  tap_surface_config_s surface_config = {0};
  surface_config.cols = 80;
  surface_config.rows = 24;
  surface_config.pixel_width = 720;
  surface_config.pixel_height = 432;
  surface_config.scrollback = 1000;

  tap_surface_t *surface = tap_surface_new(engine, &surface_config);
  if (!surface) {
    fprintf(stderr, "surface creation failed\n");
    return 1;
  }

  usleep(400 * 1000);
  const char *cmd = "printf '%s%s\\n' li btap-cgate\r";
  tap_surface_text(surface, cmd, strlen(cmd));

  tap_render_state_t *state = tap_render_state_new(surface);
  int found = wait_for(state, "libtap-cgate");

  /* Paste into a cat child: no line editor in the way, so the check does
   * not depend on the host's shell. The tty's own echo puts it on screen. */
  tap_surface_config_s paste_config = surface_config;
  paste_config.shell = "/bin/cat";
  tap_surface_t *paste_surface = tap_surface_new(engine, &paste_config);
  tap_render_state_t *paste_state =
      paste_surface ? tap_render_state_new(paste_surface) : NULL;
  int found_paste = 0;
  int found_bracketed = 0;
  int found_focus = 0;
  if (paste_surface) {
    const char *pasted = "libtap-pgate";
    tap_surface_paste(paste_surface, pasted, strlen(pasted));
    found_paste = wait_for(paste_state, "libtap-pgate");

    /* Enable mode 2004 through the child: cat copies the sequence back to
     * its stdout, which the terminal parses. The next paste must then wrap
     * in markers, echoed as ^[[200~..^[[201~. */
    const char *decset = "\x1b[?2004h\n";
    tap_surface_text(paste_surface, decset, strlen(decset));
    for (int attempt = 0; attempt < 200; attempt++) {
      if (tap_surface_mode_bits(paste_surface) & (1u << 3)) {
        break;
      }
      usleep(25 * 1000);
    }
    const char *bracketed = "libtap-bgate";
    tap_surface_paste(paste_surface, bracketed, strlen(bracketed));
    found_bracketed = wait_for(paste_state, "[200~libtap-bgate");

    /* Focus events (mode 1004) the same way. Until cat has copied the
     * sequence back, a focus change reports nothing, so keep toggling
     * until one report is echoed as ^[[O. */
    const char *focus = "\x1b[?1004h\n";
    tap_surface_text(paste_surface, focus, strlen(focus));
    for (int attempt = 0; attempt < 200 && !found_focus; attempt++) {
      tap_surface_set_focus(paste_surface, true);
      tap_surface_set_focus(paste_surface, false);
      tap_render_state_update(paste_state);
      uint16_t lines = tap_render_state_lines(paste_state);
      uint16_t cols = tap_render_state_columns(paste_state);
      for (uint16_t line = 0; line < lines && !found_focus; line++) {
        char text[512] = {0};
        uint16_t limit = cols < 511 ? cols : 511;
        for (uint16_t col = 0; col < limit; col++) {
          tap_cell_s cell = tap_render_state_cell(paste_state, line, col);
          text[col] = (cell.codepoint >= 32 && cell.codepoint < 127)
                          ? (char)cell.codepoint
                          : ' ';
        }
        found_focus = strstr(text, "^[[O") != NULL;
      }
      usleep(25 * 1000);
    }
  }

  tap_cursor_s cursor = tap_render_state_cursor(state);
  /* No program drew an image: the placement list is empty and its
   * generation unchanged. Links the image API in the process. */
  size_t images = tap_render_state_image_count(state);
  uint64_t images_generation = tap_render_state_image_placements_generation(state);
  tap_image_placement_s image_placement;
  int image_resolved =
      tap_render_state_image_placement(state, 0, 8.0f, 16.0f, &image_placement);
  printf("wakeups=%d cursor=%u,%u found=%d found_paste=%d found_bracketed=%d "
         "found_focus=%d images=%zu images_generation=%llu image_resolved=%d\n",
         atomic_load(&wakeups), cursor.line, cursor.column, found,
         found_paste, found_bracketed, found_focus, images,
         (unsigned long long)images_generation, image_resolved);

  if (paste_state) {
    tap_render_state_free(paste_state);
  }
  if (paste_surface) {
    tap_surface_free(paste_surface);
  }
  tap_render_state_free(state);
  tap_surface_free(surface);
  tap_engine_free(engine);

  if (!found || !found_paste || !found_bracketed || !found_focus ||
      atomic_load(&wakeups) == 0 || images != 0 || images_generation != 0 ||
      image_resolved) {
    fprintf(stderr, "gate failed\n");
    return 1;
  }
  printf("libtap c gate passed\n");
  return 0;
}
