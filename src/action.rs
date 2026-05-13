use serde::{Deserialize, Serialize};
use strum::Display;

#[derive(Debug, Clone, PartialEq, Eq, Display, Serialize, Deserialize)]
pub enum Action {
    Tick,
    Render,
    Resize(u16, u16),
    Suspend,
    Resume,
    Quit,
    ClearScreen,
    Error(String),
    Help,
    /// Switch the active node profile to the named `[[nodes]]`
    /// entry. Emitted by the S15 Fleet screen's Enter binding so
    /// the operator can hop from a fleet row to that node's
    /// per-screen view; App's `handle_actions` calls
    /// `switch_context` and clears the action.
    SwitchContext(String),
}
