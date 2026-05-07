# Drill panes

Stub — the S2 (stamps) and S6 (peers) drill panes share a state machine: `Idle | Loading | Loaded | Failed`. Selection state plus an internal `mpsc::UnboundedReceiver` for results. Late results from cancelled drills are dropped silently. See `tests/s2_stamps_drill.rs` and `tests/s6_peers_drill.rs` for the pure compute fn snapshot tests.
