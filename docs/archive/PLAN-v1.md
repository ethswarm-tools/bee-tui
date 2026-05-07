# bee-tui — Production Plan v1 (SUPERSEDED)

> **This plan is superseded by [`../PLAN.md`](../PLAN.md) (v2).**
> Kept for historical reference. Key changes in v2:
> - Pivoted to single-node-deep for v0.1 (multi-node moves to v0.4) based on operator pain research
> - Added Validation Gate (Discord interviews) before any code, based on competitor TUI graveyard
> - Stamps screen leads with volume+duration, not depth+amount (community is retiring depth+amount per #4992)
> - Added Health screen, Lottery screen, NAT screen, RPC screen — driven by GitHub issue evidence
> - Updated stack to ratatui 0.30 + crossterm 0.29 + color-eyre + config 0.15 + the official component template
> - Architecture switched from gitui-style hybrid to the official `cargo generate ratatui/templates component` pattern

---

## 1. Product framing

**One sentence:** bee-tui is a k9s-style operator cockpit for Ethereum Swarm Bee — multi-node, live, keyboard-first, single static binary.

**Audience:** Bee node operators (gateway, storage, light) running headless over SSH; secondary: developers integrating Bee who want a live introspection tool while building.

**Non-goals:** Replacing swarm-cli for one-shot uploads, replacing bee-dashboard for casual web browsing, key/identity management, manifest authoring.

**Success criteria for v1.0:**
- Single `cargo install bee-tui` works on Linux/macOS/Windows
- Connects to N nodes from config, runs unattended over SSH for 24h without leaks
- Stamp lifecycle, peer drilldown, upload tracker, PSS/GSOC console all functional
- Mentioned in awesome-swarm

## 2. Prerequisite work in bee-rs (close before starting v0.1)

Address as **bee-rs v1.3.0** before starting bee-tui.

| # | Gap | Effort | Why it blocks |
|---|---|---|---|
| 1 | `/chunks/stream` WebSocket upload | Medium (~150 LOC) | Live upload tracker (killer feature) |
| 2 | `/rchash/{depth}/{a1}/{a2}` endpoint | Small (~30 LOC) | Redistribution debug screen |
| 3 | `/timesettlements` endpoint | Small (~25 LOC) | Time-settlement screen |
| 4 | `Client::with_token(url, token)` ergonomic constructor | Small | Token-protected nodes |
| 5 | `tracing::instrument` on `Inner::send` | Small (optional) | TUI debug pane shows API timings |
| 6 | `Client::ping() -> Duration` helper | Trivial | Connection-status bar |

WebSocket plumbing already exists (`tokio-tungstenite 0.24` in `pss/mod.rs`, `gsoc/mod.rs`). Item 1 inverts the existing `Subscription` pattern from receiver to sender.

## 3. Architecture (v1)

### Stack

```
ratatui 0.30 (no default features, +crossterm)
crossterm 0.29
tokio 1 (full)
tokio-util  (CancellationToken)
bee-rs >= 1.3.0
color-eyre 0.6
tracing + tracing-subscriber + tracing-appender
figment 0.10 (TOML + env layers)
directories 6 (XDG paths)
indexmap (stable peer iteration)
tui-input 0.14 (command bar)
tui-tree-widget (Mantaray drilldown)
tui-popup 0.6 (modal dialogs)
throbber-widgets-tui (upload spinners)
insta (snapshot tests)
wiremock (fake Bee responses)
assert_cmd (binary smoke tests)
cargo-dist (releases)
MSRV 1.85 (matches bee-rs)
```

### Pattern: Hybrid Elm + gitui-style components

> v2 update: The community has converged on the official `component` template from `ratatui/templates`. This hybrid pattern is no longer the recommended starting point.

Pure Elm bloats past 5 screens; pure components fragment ownership. Combine: a single `Msg` enum + `update()` for global state, `Component` trait for screen-local rendering and input routing.

### Module layout (v1 proposal)

```
bee-tui/
  Cargo.toml
  src/
    main.rs              # tokio::main, panic hook, terminal lifecycle
    app.rs               # App, run_loop, tokio::select! over channels
    event.rs             # Event enum, terminal event task
    message.rs           # Msg enum (input + API results), update()
    environment.rs       # Environment{theme, keys, tx_msg, clients}
    components/
      mod.rs             # Component trait + EventState (gitui-style)
      cluster/           # multi-node overview (default screen)
      node/              # single-node detail (drill from cluster)
      peers/             # /status/peers + drill-down
      stamps/            # postage batches with TTL countdown
      uploads/           # tags + chunks/stream WS
      cheques/           # accounting + settlements
      topology/          # kademlia bins + neighborhoods
      pss/               # PSS + GSOC console
      logs/              # /loggers + tracing tail
      overlay/           # help, command bar, confirm, popup_stack
    api/
      mod.rs             # ApiClient wrapping bee_rs::Client
      poll.rs            # spawn_poll(endpoint, watch_tx, interval)
      stream.rs          # spawn_ws(subscription, mpsc_tx)
    state/
      mod.rs             # typed snapshots
      cluster.rs
      node.rs
      stamp.rs
    config.rs            # figment + TOML, profiles, themes, keys
    theme.rs             # 4-color severity palette + ASCII fallback
    tracing.rs           # file appender to $XDG_DATA_HOME/bee-tui/log
    keymap.rs            # configurable bindings
    ui/
      sparkline.rs       # braille-only, ASCII fallback
      table.rs           # sortable, paginated
      progress.rs        # stamp TTL bar, upload progress
```

### Channels (v1)

- **`tx_msg: mpsc::UnboundedSender<Msg>`** — single ingest. Input task, API tasks, ticker all push `Msg`. One queue, no fan-in confusion.
- **`tokens: HashMap<ScreenId, CancellationToken>`** — one per screen; switching screens calls `tokens[old].cancel()`. Bee has 30–60s feed lookups; without this you queue requests forever.
- **`watch::Sender<Snapshot>` per long-poll source** — last-value semantics for `/topology`, `/balances`, `/status` snapshots; readers see latest, no backpressure.
- **No `broadcast`. No `crossbeam`** — gitui uses crossbeam only because they predate tokio adoption.

### Frame budget (v1)

- Redraw on every `Msg`, **coalesced to 16 fps floor** (60ms `sleep_until`)
- Idle ticker every **1s** (Bee state changes faster than git's 5s)
- Drain input with `while event::poll(Duration::ZERO)?` to avoid scroll lag (atuin pattern)

> v2 update: Switched to two intervals — tick 250ms (logic) + render 16-33ms (60fps). Atuin and Television both do this. Render-rate independent of tick rate is the difference between snappy and sluggish.

## 4. Screens (v1.0 set, v1)

> v2 update: Reordered and added screens based on operator pain research. Health/Stamps/SWAP/Lottery now lead. Multi-node deferred.

### S1. Cluster (default screen)
- Rows = configured nodes, cols = `connectedPeers`, `pullsyncRate`, `storageRadius`, `reserveSize`, Δ(`block` vs `chainTip`), `isReachable`, `isWarmingUp`, last latency
- Source: `/status` + `/chainstate` per node, polled @ 1s
- Keys: `↵` drill into node, `s` sort col, `r` reconnect, `:` command

### S2. Node detail
- 4 panels: header (overlay/version/uptime), `/status` gauges, sparklines (peer count + sync rate over 5min), recent log tail
- Source: `/status`, `/node`, `/health`, last 200 entries from `/loggers`
- Keys: `1-9` switch sub-tab, `e` expand panel fullscreen (bottom pattern)

### S3. Peers
- Split-pane: top = sortable peer table (`/status/peers` cols: address, fullNode, proximity, syncRate, reserveSize); bottom = drill-down on selected
- Drill-down: `/balances/{addr}`, `/settlements/{addr}`, `/chequebook/cheque/{peer}`, kademlia bin from `/topology`
- Keys: `x` disconnect, `b` blocklist, `p` pingpong, `c` cashout cheque

### S4. Stamps
- Row per batch: TTL countdown bar, utilization %, depth, immutable flag, label, bucket-collision sparkline
- Source: `/stamps` polled @ 5s, `/stamps/{id}/buckets` on focus
- Keys: `t` topup, `d` dilute, `b` buy new, `↵` view buckets

### S5. Uploads
- Active tags table: cols = uid, label, split/sent/synced progress bars, ETA
- Source: `/tags` polled + WS `/chunks/stream` for live chunk feed
- Keys: `↵` open tag, `p` pin reference on completion, `c` cancel (best-effort)

### S6. Accounting
- Two tables: balances per peer (debt/credit, last update), recent settlements (peer, amount, tx)
- Source: `/balances`, `/consumed`, `/settlements`, `/timesettlements`, `/chequebook/balance`
- Keys: `c` cashout, `d` deposit, `w` withdraw

### S7. Topology
- Visual: kademlia bins as rows, depth marker, neighborhood reserve density bars
- Source: `/topology`, `/status/neighborhoods`, `/reservestate`, `/redistributionstate`
- Keys: `r` rchash compute, `e` expand fullscreen

### S8. PSS / GSOC console
- Chat-shaped scrollback per subscribed topic/address
- Source: WS `/pss/subscribe/{topic}`, WS `/gsoc/subscribe/{address}`
- Keys: `+` subscribe, `-` unsubscribe, `s` send PSS

### S9. Mantaray browser
- Tree drilldown: ref → forks → entries → chunks (dive pattern, `tui-tree-widget`)
- Source: `/bzz/{ref}/{path}` recursive, `/stewardship/{ref}` for status
- Keys: `↵` expand, `p` pin, `r` re-upload via stewardship

## 5. Roadmap (v1)

| Version | Scope | Estimated effort |
|---|---|---|
| **bee-rs 1.3.0** | 3 endpoint gaps + WS upload + auth ergonomics + ping helper | 1 week |
| **bee-tui v0.1** | Single-node, screens S1+S2+S3 (cluster, node detail, peers); config; tracing; CI | 2 weeks |
| **bee-tui v0.2** | S4 stamps + S5 uploads (with WS) + theme system | 1 week |
| **bee-tui v0.3** | Multi-node config + cluster sortable table + cancellation tokens proven under load | 1 week |
| **bee-tui v0.4** | S6 accounting + S7 topology | 1 week |
| **bee-tui v0.5** | S8 PSS/GSOC + S9 Mantaray browser; full insta snapshot suite | 1.5 weeks |
| **bee-tui v0.9** | Polish, docs, vhs recordings, cargo-dist setup, beta testing | 1 week |
| **bee-tui v1.0** | First stable release; awesome-swarm PR; blog post | release-only |

**Total: ~8.5 weeks** of focused work, including bee-rs prerequisites.

## 6. Risks & mitigations (v1)

| Risk | Mitigation |
|---|---|
| Bee API changes between minor versions | Pin bee-rs minor in Cargo.toml; show API version in header bar; warn-banner on mismatch |
| WS reconnect storms on flaky networks | Exponential backoff, max 5 retries, banner "stream lost — press R to retry" |
| 30–60s feed lookups freeze a screen | Per-screen `CancellationToken`, throbber widget, request-in-flight indicator |
| Operators on Windows Terminal with broken Unicode | `--ascii` flag falls back to ASCII box-drawing and block characters |
| Token leaks in logs | Tracing redacts headers; never log full URL with auth |
| Single binary > 20 MB scares users | Build with `lto = "fat"` + `codegen-units = 1` + `strip = true` + UPX optionally |
| TUI is fun to start, hard to maintain | Snapshot tests + wiremock integration tests gate every PR |

> v2 update: Added "Niche too small to matter (lntop-style fade)" as the top risk — and a Validation Gate to mitigate it.
