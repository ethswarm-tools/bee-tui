# Adding a screen

A practical walkthrough of adding a new screen to the
cockpit. The workflow is the same one every existing screen
followed: snapshot type → watcher → component → pure view
fn → insta tests → wire into App.

The example in this page is hypothetical — adding an "S11
— Settlements forensics" screen.

## 1. Define the snapshot type

In `src/watch/mod.rs`, add a struct holding everything one
poll of your endpoint produces:

```rust
#[derive(Debug, Clone, Default)]
pub struct SettlementsForensicsSnapshot {
    pub last_update: Option<Instant>,
    pub settlements: Vec<Settlement>,
    pub total_received: String,
    pub total_sent: String,
}
```

The `last_update: Option<Instant>` field is the convention
for "did we ever poll yet?" — components use it to
distinguish cold-start (Unknown) from "loaded but empty".

## 2. Add it to `BeeWatch`

Spawn a watcher task. The pattern (in `src/watch/`):

```rust
impl BeeWatch {
    pub fn settlements_forensics(&self) -> watch::Receiver<SettlementsForensicsSnapshot> {
        self.settlements_forensics_rx.clone()
    }
}

fn spawn_settlements_forensics_watcher(
    api: Arc<ApiClient>,
    cancel: CancellationToken,
) -> watch::Receiver<SettlementsForensicsSnapshot> {
    let (tx, rx) = watch::channel(SettlementsForensicsSnapshot::default());
    tokio::spawn(async move {
        let bee = api.bee();
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = interval.tick() => {
                    if let Ok(s) = bee.debug().settlements().await {
                        let _ = tx.send(SettlementsForensicsSnapshot {
                            last_update: Some(Instant::now()),
                            settlements: s.peers,
                            total_received: format_bzz(s.total_received),
                            total_sent: format_bzz(s.total_sent),
                        });
                    }
                }
            }
        }
    });
    rx
}
```

Pick the cadence based on how fast the data actually
changes. Settlement state changes at chain rate — 30 s is
plenty.

Wire `spawn_settlements_forensics_watcher` into
`BeeWatch::start` and store the receiver on `BeeWatch`.

## 3. Define the View struct

In a new file `src/components/settlements_forensics.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettlementsForensicsView {
    pub rows: Vec<SettlementRow>,
    pub totals: SettlementsTotals,
    pub status: SettlementsStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettlementRow {
    pub peer_short: String,
    pub received: String,
    pub sent: String,
    pub net: String,
    pub net_sign: NetSign,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetSign { Positive, Negative, Zero }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettlementsStatus {
    Unknown, // cold start
    Healthy,
    Skewed,  // some peer's |net| > threshold
}
```

The View carries **display-ready data**: pre-formatted
strings, classified statuses, sort order. The renderer
should never have to re-compute "is this row skewed" — the
view fn already did it.

## 4. Write the pure view_for fn

```rust
pub fn view_for(snap: &SettlementsForensicsSnapshot) -> SettlementsForensicsView {
    if snap.last_update.is_none() {
        return SettlementsForensicsView {
            rows: vec![],
            totals: SettlementsTotals::default(),
            status: SettlementsStatus::Unknown,
        };
    }

    let mut rows: Vec<SettlementRow> = snap.settlements
        .iter()
        .map(SettlementRow::from)
        .collect();
    rows.sort_by_key(|r| Reverse(r.abs_net_plur()));

    let any_skewed = rows.iter().any(|r| r.is_skewed());

    SettlementsForensicsView {
        rows,
        totals: SettlementsTotals { /* ... */ },
        status: if any_skewed { Skewed } else { Healthy },
    }
}
```

Pure: takes `&Snapshot`, returns `View`. No I/O, no
references to global state, no theme calls. This is the
testable surface.

## 5. Write insta snapshot tests

In `tests/s11_settlements_forensics.rs`:

```rust
use bee_tui::components::settlements_forensics::*;
use bee_tui::watch::SettlementsForensicsSnapshot;
use std::time::Instant;

fn fixture(/* parameters */) -> SettlementsForensicsSnapshot {
    SettlementsForensicsSnapshot {
        last_update: Some(Instant::now()),
        settlements: vec![/* fixture data */],
        total_received: "BZZ 12.5".into(),
        total_sent: "BZZ 11.2".into(),
    }
}

#[test]
fn cold_start_is_unknown() {
    let snap = SettlementsForensicsSnapshot::default();
    let view = view_for(&snap);
    assert_eq!(view.status, SettlementsStatus::Unknown);
}

#[test]
fn skewed_when_one_peer_is_far_out_of_balance() {
    let snap = fixture(/* ... */);
    let view = view_for(&snap);
    insta::assert_yaml_snapshot!(view);
}
```

Run `cargo test --test s11_settlements_forensics` and use
`cargo insta review` to accept the new snapshots. The
snapshots become the contract — any future change that
alters the View needs explicit re-acceptance.

## 6. Implement the Component

```rust
pub struct SettlementsForensics {
    rx: watch::Receiver<SettlementsForensicsSnapshot>,
    snapshot: SettlementsForensicsSnapshot,
    selected: usize,
    scroll_offset: usize,
}

impl SettlementsForensics {
    pub fn new(rx: watch::Receiver<SettlementsForensicsSnapshot>) -> Self {
        let snapshot = rx.borrow().clone();
        Self { rx, snapshot, selected: 0, scroll_offset: 0 }
    }
}

impl Component for SettlementsForensics {
    fn update(&mut self, action: Action) -> Result<Option<Action>> {
        match action {
            Action::Tick => self.snapshot = self.rx.borrow().clone(),
            // handle screen-specific keys here
            _ => {}
        }
        Ok(None)
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        let view = view_for(&self.snapshot);
        // render view into ratatui widgets
        Ok(())
    }
}
```

## 7. Wire into App

In `src/app.rs`:

```rust
const SCREEN_NAMES: &[&str] = &[
    "Health", "Stamps", "Swap", "Lottery", "Warmup",
    "Peers", "Network", "API", "Tags", "Log",
    "Settlements",  // NEW
];

fn build_screens(api: &Arc<ApiClient>, watch: &BeeWatch) -> Vec<Box<dyn Component>> {
    // ...existing...
    let settlements_forensics = SettlementsForensics::new(
        watch.settlements_forensics(),
    );
    vec![
        // ...existing...
        Box::new(settlements_forensics),
    ]
}
```

If your screen has screen-specific keys, add them to
`screen_keymap()`:

```rust
fn screen_keymap(active_screen: usize) -> &'static [(&'static str, &'static str)] {
    match active_screen {
        // ...existing...
        10 => &[
            ("↑↓ / j k", "scroll one row"),
            ("PgUp / PgDn", "scroll ten rows"),
        ],
        _ => &[],
    }
}
```

## 8. (Optional) Add a `:settlements` jump command

In the command bar handler in `src/app.rs`, the SCREEN_NAMES
table makes `:settlements` automatically work — any name in
the list becomes a valid screen-jump command. So nothing to
add.

## 9. Add a screens entry in mdBook

Edit `docs/book/src/SUMMARY.md`:

```markdown
- [S11 — Settlements forensics](./screens/s11-settlements.md)
```

Then write `docs/book/src/screens/s11-settlements.md`
following the existing pattern: "Why this screen exists →
data shape → status semantics → common scenarios → snapshot
cadence → keys".

## Checklist

Before opening a PR:

- [ ] Watcher task respects `cancel.cancelled()` so it
      shuts down cleanly
- [ ] Watcher cadence is appropriate (don't poll faster
      than data actually changes)
- [ ] `view_for` is pure (no `theme::active()` calls; let
      the renderer do colour)
- [ ] insta tests cover cold-start (Unknown) + healthy + at
      least one degraded state
- [ ] `cargo fmt && cargo clippy --all-targets --all-features -- -D warnings` clean
- [ ] mdBook page added to SUMMARY.md
- [ ] If your screen has interactive keys, they're listed
      in `screen_keymap()` so the `?` overlay finds them

## Things to *not* do

- **Don't poll inside a Component.** Components are pure
  renderers. Move polling to the watch hub.
- **Don't share mutable state between Components.** Use
  the watch hub if multiple screens need the same data.
- **Don't compute layout / colour inside `view_for`.** That
  belongs in `draw`. The View is data, the renderer is
  presentation.
- **Don't skip insta tests** even if the screen "looks
  simple". The investment pays off the first time someone
  refactors the cockpit's wiring.
