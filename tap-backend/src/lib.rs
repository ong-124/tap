// The terminal core (VT state machine, parser, grid, selection, search,
// PTY event model, colors) now lives in the `tap-vt` crate. Re-export
// every module at its original `crate::`/`tap_backend::` path so nothing
// downstream (tapterm, tests) has to change. This crate keeps the
// user-facing config and the sugarloaf renderer glue.
pub use tap_vt::{
    ansi, clipboard, codepoint_width, crosswords, error, event, performer, selection,
    simd_base64, simd_utf8,
};

pub mod config;

#[cfg(test)]
mod graphics;

#[cfg(feature = "renderer")]
pub use sugarloaf;
