//! Re-export stub. The webhook health-gate alerter moved to
//! [`bee_cockpit_core::alerts`] during the cockpit-core extraction;
//! this stub preserves the `crate::alerts::*` paths inside bee-tui.

pub use bee_cockpit_core::alerts::*;
