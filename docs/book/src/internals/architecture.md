# Architecture

How the cockpit is wired internally. For developers /
contributors / anyone reading the source. The design rules
optimise for **predictable rendering**, **clean shutdown**,
and **testability** — in that order.

## The two-layer model

```
┌──────────────────────────────────────────────┐
│  COMPONENTS (per-screen)                     │
│   - hold a watch::Receiver<T>                │
│   - implement view_for(snap) -> View         │
│   - render the View into ratatui widgets    │
└──────────────────────────────────────────────┘
              ▲
              │ tokio::sync::watch
              │
┌──────────────────────────────────────────────┐
│  WATCH HUB (BeeWatch)                        │
│   - one tokio task per Bee endpoint          │
│   - each task owns a watch::Sender<T>        │
│   - all tasks under a CancellationToken      │
└──────────────────────────────────────────────┘
              ▲
              │
        ApiClient (bee-rs)
```

The watch hub is the *single source of truth* for live data.
Each component is a *pure renderer* that takes the latest
snapshot and computes a `View` struct, which gets rendered.

## The watch hub (`src/watch/`)

`BeeWatch::start(api, root_cancel)` spawns one tokio task
per resource. Each task:

- Holds an `Arc<ApiClient>`
- Owns a `tokio::sync::watch::Sender<T>` for its resource
- Polls the relevant Bee endpoint at a fixed cadence
- Calls `tx.send(new_snapshot)` on each tick

Resources currently watched (with cadence):

| Resource | Endpoint(s) | Cadence |
|---|---|---|
| Health | `/status`, `/wallet`, `/chainstate`, `/redistributionstate` | 2 s |
| Topology | `/topology` | 5 s |
| Stamps | `/stamps` | 5 s |
| Swap | `/chequebook/balance`, `/chequebook/cheque`, `/settlements`, `/timesettlements`, `/chequebook/address` | 30 s |
| Lottery | `/redistributionstate`, `/stake` | 30 s |
| Tags | `/tags` | 5 s |
| Network | `/addresses` | 60 s |
| Transactions | `/transactions` | 5 s |

Cadences are tuned for the rate at which each resource
*actually changes*. Stamps utilization grows at upload rate
— 5 s is plenty. Settlement cheques change at chain rate —
30 s. Underlay addresses essentially never change — 60 s.
Hammering Bee at 1 s for everything would burn CPU on both
sides.

## Cancellation

Every watcher inherits from a single `tokio_util::sync::CancellationToken`
called `root_cancel`, owned by `App`. On quit:

1. `App::run()` flips `should_quit = true`
2. `App::run()` calls `root_cancel.cancel()`
3. Every watcher task's loop sees the cancellation and exits
4. The terminal is restored
5. Process exits cleanly

`:context <name>` is the same pattern, scoped: the active
`BeeWatch::shutdown()` cancels its children, a new
`BeeWatch::start(new_api, &self.root_cancel)` spawns under
the same root, and component receivers are rebuilt.

This means **no orphaned tasks** can outlive the cockpit.
Even mid-fetch drill spawns are tied to the same tree —
they get cancelled at quit / context-switch and never
silently complete.

## Components (`src/components/`)

One file per screen. Each file:

```rust
pub struct MyScreen {
    rx: watch::Receiver<MySnapshot>,
    snapshot: MySnapshot,
    // screen-local state (cursor, drill, etc.)
}

impl MyScreen {
    pub fn view_for(snap: &MySnapshot) -> MyView {
        // pure: snap → view, no I/O
    }
}

impl Component for MyScreen {
    fn update(&mut self, action: Action) -> Result<Option<Action>> {
        if matches!(action, Action::Tick) {
            self.snapshot = self.rx.borrow().clone();
        }
        Ok(None)
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        let view = Self::view_for(&self.snapshot);
        // render view into ratatui widgets
    }
}
```

The `view_for` separation is the cockpit's testability
trick: `tests/sN_*.rs` files load fixture snapshots, call
`view_for`, and assert against insta snapshots — without
launching a TUI.

## Drill panes (`src/components/peers.rs`, `stamps.rs`)

Drills are *fire-and-forget* spawns inside a component, not
new watchers in the hub. The pattern:

```rust
enum DrillState {
    Idle,
    Loading { ... },
    Loaded { view: ... },
}

struct MyComponent {
    drill: DrillState,
    drill_rx: mpsc::UnboundedReceiver<DrillResult>,
    drill_tx: mpsc::UnboundedSender<DrillResult>,
    // ...
}
```

When the user presses `↵`:
1. Spawn a tokio task that fans out 4 endpoint fetches in
   parallel via `tokio::join!`
2. Send the aggregate result down `drill_tx`
3. On next `Tick`, drain `drill_rx` and update `drill` state
4. Render reads `drill`

A second `↵` while a drill is loading is a no-op (we just
re-target the same Loading state). `Esc` clears `drill` to
`Idle` and ignores any late results.

See [Drill panes](./drills.md) for the full pattern.

## Pure-fn rendering for testability

Every screen has a `view_for` (or `compute_*_view`) function
that takes a snapshot and produces a `View` struct of
display-ready data: pre-formatted strings, classified
statuses, sorted rows. The `Component::draw` method only
turns `View` into ratatui widgets.

This means **snapshot tests don't need a TUI**:

```rust
#[test]
fn s2_critical_immutable_batch() {
    let snap = StampsSnapshot {
        batches: vec![/* fixture */],
        ..Default::default()
    };
    let view = Stamps::view_for(&snap);
    insta::assert_yaml_snapshot!(view);
}
```

The `tests/sN_*.rs` files are entirely TUI-free. They run
in CI in <1 s each. When adding behaviour, write the test
against `view_for` first — the renderer follows.

## Action / Tick loop (`src/action.rs`, `src/app.rs`)

The cockpit has a single `Action` enum that drives every
component:

```rust
pub enum Action {
    Tick,
    Render,
    Quit,
    Suspend,
    Resume,
    ClearScreen,
    Resize(u16, u16),
    // ...
}
```

`App::run()` is a simple loop:

```
loop {
    handle terminal events → push Actions onto a channel
    handle cancellation → break
    drain action channel → dispatch to components
    render
}
```

Components return `Option<Action>` from `update()` — a
follow-up action that gets pushed back onto the channel.
This is the only inter-component communication path; there
are no direct mutable references between components. (The
shared *data* lives in the watch hub, not in components.)

## Theme system (`src/theme.rs`)

A global `Theme` (palette + glyphs) installed once at
startup via `theme::install_with_overrides(...)`. Components
read it via `theme::active()`. Hot-reload isn't supported
by design — the cost of supporting it (locking, redraw on
change) outweighs the benefit (set the theme once and
forget).

See [Theme & accessibility](../reference/theme.md) for the
slot-based palette + glyphs design.

## API client (`src/api/`)

A thin `ApiClient` wrapper over bee-rs. Holds:

- The Bee endpoint URL
- The Bearer token (resolved from `@env:VAR` at startup)
- The profile name

The wrapper is `Arc<ApiClient>` and gets cloned into every
watcher task and drill spawn. `:context` switching builds a
new `Arc<ApiClient>` and rebuilds the screen list against
it; old fan-out spawns die with the old root cancel.

## Logging (`src/logging.rs`, `src/log_capture.rs`)

`tracing` + a process-wide `LogCapture` ring buffer
(capacity 200). Every bee-rs HTTP call emits a structured
event captured here. S10 (the command log) renders the
buffer; `:diagnose` dumps the last 50 entries; S8's call
stats compute p50/p99 over the most recent 100.

Tokens are *never* in the buffer — only `method`, `url`,
`status`, `elapsed_ms`, `ts`. Headers (where Bearer lives)
are not captured.

## Where to read for more depth

- The `docs/PLAN.md` (in the repo) is the canonical
  pre-implementation design doc — § 6 has the watch-hub
  design in full
- The `tests/sN_*.rs` files show how each `view_for` is
  tested — useful when adding a new screen
- The `src/components/peers.rs` file is the most complex
  component (bin saturation strip + scrollable peer table
  + 4-way drill); it's the canonical example of "everything
  the cockpit can do"
