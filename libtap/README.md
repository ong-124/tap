# libtap

Embeddable terminal core extracted from [Tap](https://github.com/raphamorim/tap):
PTY spawning, VT parsing, terminal state, and a host-pulled render-state API
with per-row dirty tracking. No drawing or windowing code — bring your own
renderer.

`libtap` wraps [`tap-vt`](../tap-vt) as a C ABI for non-Rust consumers (Swift,
C, and anything that speaks C). It is **not published to crates.io**: it lives
in the Tap source tree and ships as the `TapKit.xcframework` static library in
Tap's GitHub releases. If you write Rust, depend on `tap-vt` directly.

The Rust surface it exposes over the C boundary:

```rust
use libtap::{Engine, RenderState, SurfaceDelegate, SurfaceDesc};
use std::sync::Arc;

struct Delegate;
impl SurfaceDelegate for Delegate {
    fn wakeup(&self, _surface: usize) { /* schedule a render */ }
}

let engine = Engine::new(Arc::new(Delegate));
let surface = engine.create_surface(&SurfaceDesc::default())?;
let mut state = RenderState::new(&surface);
surface.text("ls\r");
state.update();
```

The C ABI is compiled by default (`crate-type = ["staticlib"]`); see
`libtap/include` for the curated header and modulemap, and the `Makefile`
`libtap-xcframework` target for packaging.
