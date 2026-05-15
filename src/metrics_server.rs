//! Re-export stub. The single-route `/metrics` HTTP listener moved
//! to [`bee_cockpit_core::metrics_server`]; this stub preserves the
//! `crate::metrics_server::*` paths inside bee-tui.

pub use bee_cockpit_core::metrics_server::*;
