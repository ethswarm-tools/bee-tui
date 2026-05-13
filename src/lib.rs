//! bee-tui — internal library exposing the modules the binary
//! composes. Living as both a `[lib]` and a `[[bin]]` lets integration
//! tests in `tests/` exercise the cockpit's components against
//! fixtures, without launching the full TUI loop.
//!
//! Public exports are stable for tests only — there is no ABI
//! commitment for downstream library users. Use the `bee-tui`
//! command-line binary, not this lib.

pub mod action;
pub mod alerts;
pub mod api;
pub mod app;
pub mod bee_log;
pub mod bee_log_tailer;
pub mod bee_log_writer;
pub mod bee_supervisor;
pub mod cli;
pub mod components;
pub mod config;
pub mod config_doctor;
pub mod durability;
pub mod economics_oracle;
pub mod errors;
pub mod feed_probe;
pub mod feed_timeline;
pub mod fleet;
pub mod log_capture;
pub mod logging;
pub mod manifest_walker;
pub mod metrics;
pub mod metrics_server;
pub mod once;
pub mod pprof_bundle;
pub mod pubsub;
pub mod stamp_preview;
pub mod state;
pub mod theme;
pub mod tui;
pub mod uploads;
pub mod utility_verbs;
pub mod version_check;
pub mod watch;
