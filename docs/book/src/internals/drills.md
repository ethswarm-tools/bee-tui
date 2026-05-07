# Drill panes

The cockpit has two on-demand drill panes: the S2 stamp
bucket drill (Enter on a batch row) and the S6 peer drill
(Enter on a peer row). Both share the same state-machine
+ async fan-out pattern. This page documents that pattern
so future drills (S9 tag drill, S3 peer-cheque drill, etc.)
can be added consistently.

## The state machine

```rust
pub enum DrillState {
    Idle,
    Loading { /* selection identifier */ },
    Loaded   { view: DrillView },
    Failed   { error: String },     // S2 only — S6 has per-row failures
}
```

Four states, one transition diagram:

```
                ↵ pressed
   Idle ─────────────────────► Loading
    ▲                              │
    │                              │ async fetch completes
    │ Esc                          ▼
    └──────────── Loaded ──────────┘
                  Failed
```

- `Idle` — regular table is rendered, no drill UI.
- `Loading` — drill pane is rendered with a spinner; data is
  in-flight.
- `Loaded` — drill pane is rendered with the result.
- `Failed` (S2 only) — drill pane shows an error message.
  S6 takes a different approach: each of the four endpoints
  can fail independently, so failure is per-row, not pane.

`Esc` always returns to `Idle`. `↵` from `Loaded` re-fires
(useful for the rchash benchmark; less common for the
drill panes).

## The async fan-out

S6's drill is the canonical example — four endpoints in
parallel:

```rust
fn start_peer_drill(&self, peer_overlay: String, bin: Option<u8>) {
    let api = self.client.clone();
    let tx = self.drill_tx.clone();
    tokio::spawn(async move {
        let bee = api.bee();
        let debug = bee.debug();
        let (balance, cheques, settlement, ping) = tokio::join!(
            debug.peer_balance(&peer_overlay),
            debug.peer_cheques(&peer_overlay),
            debug.peer_settlement(&peer_overlay),
            debug.pingpong(&peer_overlay),
        );
        let fetch = PeerDrillFetch {
            balance: balance.map_err(|e| e.to_string()),
            cheques: cheques.map_err(|e| e.to_string()),
            settlement: settlement.map_err(|e| e.to_string()),
            ping: ping.map_err(|e| e.to_string()),
        };
        let _ = tx.send((peer_overlay, fetch));
    });
}
```

Note: each endpoint result is converted to
`Result<T, String>` *before* being sent down the channel.
This is critical — it means the receiving side doesn't need
to handle each endpoint's specific error type, and the
aggregated `PeerDrillFetch` can be passed to a pure
`compute_peer_drill_view(...)` for testability.

### Why mpsc, not oneshot?

Even though only one drill is in flight at a time
conceptually, we use `mpsc::UnboundedReceiver` rather than
`oneshot`. Reason: the user can press `Esc` and `↵` again
quickly, kicking off a new fetch before the old one
completes. The new spawn sends down the same `tx`; the
receiver drains all of them.

Late results from cancelled drills are dropped silently:

```rust
fn pull_drill_results(&mut self) {
    while let Ok((peer, fetch)) = self.drill_rx.try_recv() {
        // Only consume if this matches the currently loading peer
        let pending_peer = match &self.drill {
            DrillState::Loading { peer, .. } => peer.clone(),
            _ => continue,  // drop late result
        };
        if peer != pending_peer { continue; }
        let bin = match &self.drill {
            DrillState::Loading { bin, .. } => *bin,
            _ => None,
        };
        let view = Self::compute_peer_drill_view(&peer, bin, &fetch);
        self.drill = DrillState::Loaded { view };
    }
}
```

The `continue` on drop is intentional. We don't log
"dropped a stale drill result" — it's part of normal flow.

## The pure compute fn

Like screens themselves, drills have a pure
`compute_*_drill_view(...)` that takes the fetch result and
produces a `DrillView`:

```rust
pub fn compute_peer_drill_view(
    peer: &str,
    bin: Option<u8>,
    fetch: &PeerDrillFetch,
) -> PeerDrillView {
    PeerDrillView {
        peer_overlay: peer.into(),
        bin,
        balance:   fetch.balance.as_ref()
            .map(|b| format_balance(b))
            .map_err(|e| e.clone()).into(),
        ping:      fetch.ping.clone().into(),
        // ... other fields
    }
}
```

This is the snapshot-test surface: feed it a fixture
`PeerDrillFetch` (mix of Ok and Err per field) and assert
the View renders as expected. See
`tests/s6_peers_drill.rs` for the canonical fixture set.

## Cancellation semantics

Drill spawns are *not* tied to the `root_cancel`
explicitly — they're fire-and-forget. They will always
complete (or error out via the underlying HTTP timeout).
The cockpit doesn't care; late results land on a closed
channel (silently dropped) or get filtered by the
"matches current selection" check above.

The exception: when `:context` switches profiles, the
component itself is rebuilt. The new component has a fresh
`mpsc::channel`; old in-flight spawns send to the old `tx`,
which is dropped, and their results vanish. Clean by design.

## Adding a new drill

Three pieces:

1. **A `DrillState` enum** in your component file with the
   variants you need. Reuse `Idle | Loading | Loaded` if
   the failure mode is per-pane; if it's per-row (like S6's
   four endpoints), make `DrillField<T>` like S6 does.
2. **An async spawn function** that does the fetch and
   sends the result down an internal `mpsc`. Use
   `tokio::join!` to fan out parallel fetches when possible.
3. **A pure `compute_*_drill_view(...)` fn** that takes the
   fetch result and produces a `DrillView`. Test it with
   insta snapshots covering: cold load (Loading), happy
   path (Loaded), partial failure (where applicable).

## What to *not* do

- **Don't put the drill fetch inside `update()`** — async
  doesn't work cleanly there, and you'll block tick handling.
  Always `tokio::spawn`.
- **Don't make the drill auto-refresh.** Drills are
  on-demand by design; auto-refresh would burn API calls
  on data the operator may have already left.
- **Don't make the drill block the main pane** — the
  underlying screen should keep refreshing while the drill
  is open. The drill is an overlay, not a modal lock.
- **Don't share `drill_rx` between components.** Each
  component owns its own channel. Drills are component-
  local state.

## Examples in the codebase

| File | Drill type | Endpoints |
|---|---|---|
| `src/components/stamps.rs` | Bucket histogram | `GET /stamps/<id>/buckets` (single) |
| `src/components/peers.rs` | Per-peer | `peer_balance`, `pingpong`, `peer_settlement`, `peer_cheques` (4 in parallel) |
| `src/components/lottery.rs` | rchash benchmark | `GET /rchash/<depth>/<a1>/<a2>` (single, with timing) |

The Lottery rchash isn't strictly a "drill" by name but it
follows the same pattern: state machine, async fan-out,
pure compute fn.

## See also

- [Architecture](./architecture.md) — the watch-hub +
  component-renderer pattern that drills extend
- [Adding a screen](./add-screen.md) — the broader workflow
  this page sits inside
- [`tests/s2_stamps_drill.rs`](https://github.com/ethswarm-tools/bee-tui/blob/main/tests/s2_stamps_drill.rs)
  and [`tests/s6_peers_drill.rs`](https://github.com/ethswarm-tools/bee-tui/blob/main/tests/s6_peers_drill.rs)
  in the repo for the canonical test fixtures
