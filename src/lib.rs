//! bee-tui — internal library exposing the modules the binary
//! composes. Living as both a `[lib]` and a `[[bin]]` lets integration
//! tests in `tests/` exercise the cockpit's components against
//! fixtures, without launching the full TUI loop.
//!
//! Public exports are stable for tests only — there is no ABI
//! commitment for downstream library users. Use the `bee-tui`
//! command-line binary, not this lib.

pub mod action;
pub mod api;
pub mod app;
pub mod bee_log;
pub mod bee_log_tailer;
pub mod bee_log_writer;
pub mod bee_supervisor;
pub mod cli;
pub mod components;
pub mod config;
pub mod errors;
pub mod log_capture;
pub mod logging;
pub mod state;
pub mod theme;
pub mod tui;
pub mod watch;
