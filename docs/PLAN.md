# bee-tui — Production Plan v2

**Status:** canonical. Supersedes [`archive/PLAN-v1.md`](archive/PLAN-v1.md).

**Stack:** Rust + Ratatui 0.30 + Tokio + crossterm 0.29 on top of bee-rs ≥ 1.3.0.

**One sentence:** bee-tui is the operator's truth-teller — a live terminal cockpit that surfaces the state Bee's API hides (bucket collisions, redistribution skip reasons, unhealthy gates, NAT reality).

---

## 0. What changed from v1 (read this first)

Four streams of research reshaped the plan:

| Source | Insight | Plan change |
|---|---|---|
| Operator pain points | Top 3 anxieties are **stamps, cheques, "why am I unhealthy/not earning"** — not multi-node | Pivot v0.1 to single-node-deep. Multi-node moves to v0.4 |
| Operator pain points | Bee community is actively **retiring depth+amount** in favor of volume+duration (issue #4992) | Stamp screen leads with volume+duration; depth/amount in collapsible "advanced" |
| Bee internals | `utilization` = **MaxBucketCount, not average** — the API itself is misleading | Stamp screen MUST show bucket histogram; "100% utilization" is the symptom, not the cause |
| Bee internals | `LastPlayedRound` is stale when frozen/skipped; redistribution has 4 phases per 152-block round | Lottery screen with phase-by-phase round timeline + skip reasons |
| Competitor cockpits | Lightning's lntop archived (190★ peak); ipfs-tui dead at 20★; lncli-curses dead at 7★ | Risk is real. Validation step required before coding |
| Competitor cockpits | k9s playbook on a Web3 node is **untried** — that's the actual opportunity | Adopt k9s patterns wholesale: `:command`, watch/informer split, command log |
| 2026 Ratatui SOTA | Community converged on **official `component` template** — not gitui-style, not Elm | Switch architecture: `cargo generate ratatui/templates component` as starting point |
| 2026 Ratatui SOTA | `color-eyre`, `config 0.15`, `crossterm 0.29`, `ratatui 0.30`, `tokio-tungstenite 0.29`, `tachyonfx 0.25` (now under ratatui org) | Updated BoM throughout |

---

## 1. Validation gate (do this BEFORE writing code)

The strongest signal from the prior-art survey: **every previous decentralized-storage TUI died from author-only adoption.** ipfs-tui (20★, dead), lncli-curses (7★, dead), lntop (190★ peak, archived 2025). The structural reason: most network operators run fleets behind Prometheus/Grafana, not single nodes in tmux.

**Mitigation:** Before any code, validate by posting a 60-second screenshot mockup in the `#node-operators` Discord channel (forum is dead per research) and asking five concrete questions:

1. Would you run this in tmux daily?
2. Which one screen would you actually use? (we want answers, not "all of them")
3. Are you already using bee-dashboard / Grafana? Why or why not?
4. What's the one thing you wish you could see live that you can't today?
5. Would you trust it to issue cashouts/topups/dilutes?

**Success criteria for proceeding:** ≥3 of 5 say "yes, daily" AND there's convergence on which screens matter.

**Failure criteria:** "I just curl the API + check Grafana" wins → don't build the TUI; ship a curated Grafana dashboard pack instead.

This is one week of patience that saves three months of misdirected building.

---

## 2. Product framing (sharpened)

**Audience size (honest):** ~100s–low-1000s of serious Bee operators today, of whom ~20 are highly engaged power-users (ldeffenb, crtahlin, 0xCardiE, mfw78, attila-lendvai recur across years of issues). Day-one realistic adoption: 50–200 people. This will never be a "popular" tool. It can be the *de-facto* tool for the people who matter.

**The thesis:** Bee operators are developer-leaning (gateways, debugging, exploration), not passive stakers. That's the same audience that adopted k9s and lazygit. The niche is empty because nobody has tried the **k9s playbook** on a Web3 node — every previous attempt was a viewer (lntop) or installer (EthPillar). Be the first.

---

## 3. Product principles (from research)

1. **Truth over API.** Where the API misleads (`utilization` = max-bucket, `isReachable` flickers under symmetric NAT, `LastPlayedRound` stale on skip), the TUI shows the real state with explanation. This is the #1 differentiator vs bee-dashboard.
2. **Encode tribal knowledge.** Institutional Bee wisdom lives in GitHub issue threads. Surface it inline: tooltips, micro-explanations, "why this matters" hints. Operators learn by reading the TUI.
3. **Confirmation-gated writes.** Operators have low trust (issue #5216 lost data in 1.x.y migration just weeks before research). Every state-changing action — cashout, dilute, topup, disconnect — needs a dry-run + confirm step.
4. **Transparent like lazygit.** Show the underlying HTTP request/response in a command log pane. Operators trust transparent tools.
5. **Diagnostic-bundle export.** One key produces a Discord-paste-ready snippet for `#node-operators`. Forum is dead, chat is alive — meet operators where they support each other.
6. **Watch streams, not polling-from-views.** k9s pattern: a watch/informer layer feeds an event bus; views subscribe. Don't poll inside render code.
7. **Per-view refresh policy.** tig pattern: `auto` / `periodic(Ns)` / `manual` / `after-command`. Health screen wants 2s; chunk explorer wants manual; queue wants stream.
8. **Configurable columns.** lntop pattern: gateway operator vs storage staker vs developer want different columns. Make peer/stamp tables column-configurable from `bee-tui.toml`.

---

## 4. Final scope: what's IN v1.0

Operator-pain-validated screen list, ranked by severity × frequency from research:

| # | Screen | Top pain it solves | API surface | Severity |
|---|---|---|---|---|
| 1 | **Health gates** | "Why is my node unhealthy?" (#5428, #5396) | `/health`, `/readiness`, `/redistributionstate`, `/status`, `/chainstate`, `/wallet` | Severe |
| 2 | **Stamps** (volume+duration first; bucket histogram; usable countdown) | Depth/amount confusion (#4992) + bucket collision (#4292, #3122) + "is this batch usable yet" | `/stamps`, `/stamps/{id}`, `/stamps/{id}/buckets`, `/chainstate` | Severe |
| 3 | **SWAP / cheques** (uncashed-IN list, cashout candidates, gas break-even) | Cheque cashout confusion (#4663 + 8 dupes over 4 years) | `/balances`, `/timesettlements`, `/settlements`, `/chequebook/*`, `/wallet` | Severe |
| 4 | **Lottery** (round phases, skip reasons, last 20 rounds, rchash benchmark) | "Why am I not earning rewards?" (#4849) | `/redistributionstate`, `/stake`, `/rchash` | High |
| 5 | **Warmup** (phased progress on first launch) | 25–60min cold-start opacity (#4746) | `/health`, `/readiness`, `/status`, `/topology` | High |
| 6 | **Peers + bin saturation** | Sync/topology confusion + per-bin starvation hidden in API | `/status/peers`, `/peers`, `/topology`, `/blocklist`, `/pingpong` | High |
| 7 | **Reachability/NAT** (public addrs, inbound count, AutoNAT) | "I have peers but I'm unreachable?" (#4194, #5380) | `/status`, `/topology`, `/addresses` | Medium |
| 8 | **RPC health** (block delta, error rate, pending tx queue) | RPC fragility blamed on Bee (#4419, #5388 et al) | `/chainstate`, `/transactions` | Medium |
| 9 | **Tags + uploads** (synced/seen/sent live) | Pinning large files broken (#4864), upload progress missing | `/tags`, `/tags/{uid}`, WS `/chunks/stream` | Medium |
| 10 | **Command log** (HTTP req/res tail, lazygit-style) | Trust + tutorial + "logs are broken" (#4636) | All — passive observer | Medium-foundational |

### What's OUT of v1.0

- **Multi-node** — moves to v0.4 after single-node-deep is proven loved
- **PSS/GSOC chat console** — niche; v0.5
- **Mantaray browser** — niche; v0.5
- **Identity/key management** — security-sensitive, stays in swarm-cli
- **Big file uploads** — pipe to a file from CLI, not a TUI
- **Manifest authoring / ACT grantee editing** — forms territory, swarm-cli

---

## 5. bee-rs prerequisites (bee-rs v1.3.0)

Confirmed from the audit at `bee-rs` source (v1.2.0):

| # | Gap | Effort | Why blocking |
|---|---|---|---|
| 1 | `/chunks/stream` WebSocket upload | Medium (~150 LOC) | Tags/uploads screen WS path |
| 2 | `/rchash/{depth}/{a1}/{a2}` endpoint | Small (~30 LOC) | Lottery rchash benchmark button |
| 3 | `/timesettlements` endpoint | Small (~25 LOC) | SWAP refresh-vs-cheque split |
| 4 | `Client::with_token(url, token)` constructor | Small | Token-protected nodes |
| 5 | `tracing::instrument` on `Inner::send` | Small | Command log pane gets timings free |
| 6 | `Client::ping() -> Duration` | Trivial | Connection-status bar |

Audit highlights the WS plumbing already exists in `pss/mod.rs:14`, `gsoc/mod.rs:73` (tokio-tungstenite 0.24); item 1 inverts the existing `Subscription` pattern from receiver-of-Bytes to sender-of-Bytes.

**Ship bee-rs 1.3.0 BEFORE bee-tui v0.1.** This is one week of work and it unblocks everything.

See [`research/03-bee-rs-audit.md`](research/03-bee-rs-audit.md) for the full audit.

---

## 6. Architecture (final, current to mid-2026)

### Starting point

```bash
cargo generate --git https://github.com/ratatui/templates component bee-tui
```

The official `component` template is the 2026 de-facto starting point. Every actively-developed multi-pane Ratatui app converged on this shape (television, atuin, gitui, b4n, lazyjj). Don't re-derive it.

### Architectural pattern

**k9s playbook + Ratatui component template:**

```
┌─────────────────────────────────────────────────────┐
│                       App                           │
│  - root CancellationToken                           │
│  - mpsc::UnboundedSender<Action>  (single ingest)   │
│  - Vec<Box<dyn Component>>                          │
│  - BeeWatch (watch/informer layer)                  │
└──────────────┬──────────────────────────────────────┘
               │ subscribes via watch::Receiver<T>
   ┌───────────┴───────────────┐
   │       BeeWatch            │  separate from views
   │  - per-resource pollers   │  (k9s pattern)
   │  - WS subscribers         │
   │  - publish to watch::Sender│
   └─────┬─────────────┬────────┘
         │             │
   ┌─────▼────┐  ┌─────▼────┐
   │ poll_status │ │ poll_stamps │  ...
   │ tick=2s     │ │ tick=10s    │
   │ child token │ │ child token │
   └────────────┘ └────────────┘
```

### Rules locked in

1. **`crossterm::event::EventStream`** with `StreamExt::fuse()` — drop the read-thread.
2. **Two intervals, not one.** `tick` at 250ms (logic) + `render` at 16–33ms (60fps cap). Atuin and Television both do this.
3. **`tokio::select!` directly.** No higher-level abstraction has displaced it. Skip rat-salsa unless you need its built-in dialog plumbing.
4. **`tokio_util::sync::CancellationToken` parent/child tree.** App owns root token; each long task gets a child. Quitting cancels root; switching context cancels subtree. This is what avoids atuin-style "queue requests forever" with Bee's 30–60s feed lookups.
5. **`watch::Sender<Snapshot>` per long-poll source** — last-value semantics, no backpressure.
6. **`mpsc::UnboundedSender<Action>` for inputs and triggered actions.**
7. **Stream-driven WS via `tokio-tungstenite 0.29`** — already in bee-rs for PSS/GSOC; reused for `/chunks/stream`.

### Module layout

```
bee-tui/
  Cargo.toml
  src/
    main.rs                 # tokio::main, color_eyre::install, terminal::init
    app.rs                  # App, run loop, Action enum
    action.rs               # Action variants (input + watch updates + side-effects)
    components/             # Component trait per ratatui template
      mod.rs
      health/               # screen 1 — health gates
      stamps/               # screen 2 — stamps (volume+duration UX)
      swap/                 # screen 3 — cheques + cashouts
      lottery/              # screen 4 — redistribution rounds
      warmup/               # screen 5 — phased bootstrap
      peers/                # screen 6 — peers + bin saturation
      network/              # screen 7 — NAT + reachability
      rpc/                  # screen 8 — RPC health
      uploads/              # screen 9 — tags + chunks/stream WS
      command_log/          # screen 10 — HTTP req/res tail
      overlay/
        help.rs
        command_bar.rs      # k9s :command
        confirm.rs          # write-action confirmation
        diagnostic_bundle.rs # Discord-paste export
    watch/                  # k9s-style watch/informer layer
      mod.rs
      status.rs             # /status @ 2s
      stamps.rs             # /stamps @ 10s; /buckets on focus
      redistribution.rs     # /redistributionstate @ block-rate
      chequebook.rs         # /chequebook/cheque @ 30s
      tags.rs               # /tags @ 5s
      chunks_stream.rs      # WS /chunks/stream
    api/
      client.rs             # bee_rs::Client wrapper + ping helper
      ratelimit.rs          # ensure we don't DoS our own node
    state/
      snapshot.rs           # typed cache of latest watch values
      derived.rs            # derived state (worst_bucket_eta, gas_breakeven)
    config.rs               # config 0.15 layered TOML + env
    theme.rs                # 4-color severity palette
    keymap.rs               # tig-style layered: view → generic → builtin
    tui.rs                  # terminal init/restore + panic hook
    tracing_init.rs         # tracing-appender to file, never stdout
    diagnostic.rs           # bundle generator
```

### Anti-patterns avoided (from research)

| Avoid | Why | Source |
|---|---|---|
| god `App` struct with 25+ popup fields | gitui's pain point; unmaintainable | gitui review |
| Bubble Tea-style single `Update` switch | Doesn't scale to multi-stream watch | ipfs-tui died from this |
| Polling from inside view code | Tightly couples view to backend; blocks UI | k9s separates explicitly |
| Mouse-required navigation | Operators are over SSH | universal |
| One global refresh rate | Bee state changes at 4 different rates (block, status, stamps, peers) | tig has been right since 2006 |
| Hardcoded columns | Gateway op vs storage op vs dev want different views | lntop |
| Modal dialog soup | Operators hate it | every honest review |
| Truecolor gradients | Poor contrast under SSH; color-blind hostile | btop's mistake |
| Nerd Fonts dependency | SSH-from-anywhere reality | universal |

---

## 7. Library BoM (verified mid-2026)

| Crate | Version | Purpose |
|---|---|---|
| `ratatui` | 0.30.0 | Core; modular workspace, no_std-capable |
| `crossterm` | 0.29.0 | Terminal backend |
| `tokio` | 1.52.x | Async runtime, `features = ["full"]` |
| `tokio-util` | 0.7.x | `CancellationToken`, `StreamExt` |
| `tokio-tungstenite` | 0.29.0 | WebSocket client (matches bee-rs upgrade) |
| `bee-rs` | ≥1.3.0 | API client (with prereq work shipped) |
| `color-eyre` | 0.6.5 | Errors + panic hook (replaces anyhow) |
| `tracing` + `tracing-subscriber` + `tracing-appender` | current | File logging only; never stdout |
| `config` | 0.15.x | Layered TOML+env (replaces figment) |
| `directories` | 6.x | XDG paths |
| `serde` + `toml` | current | |
| `tui-input` | 0.15.x | Command bar + form fields |
| `tui-tree-widget` | 0.24.x | Mantaray browser (v0.5) |
| `tui-popup` | 0.7.x | Confirm dialogs |
| `throbber-widgets-tui` | 0.11.x | Spinners during async ops |
| `tachyonfx` | 0.25.x | State-transition effects (now under `ratatui/` org) |
| `indexmap` | current | Stable peer iteration |
| `nucleo` | 0.5.0 | Fuzzy search across stamps/peers/contexts |
| `insta` | 1.47.x | Snapshot tests on rendered `Buffer` |
| `wiremock` | current | Fake Bee responses for integration tests |
| `assert_cmd` | current | Binary smoke tests |
| `cargo-dist` | 0.31.x | Release pipeline (still named cargo-dist on crates.io) |
| `vhs` (charmbracelet) | binary | Visual regression in CI |

**Optional:** `ratzilla 0.3.x` — bonus path to ship a WASM web build of bee-tui from the same code. Defer to v1.x.

**MSRV:** 1.85 (matches bee-rs).

---

## 8. Screen specs (operator-validated)

Each screen below names: data shown, API source, refresh policy, key actions, and the *operator anxiety it answers*.

### S1. Health gates (default screen on launch)

**Anxiety:** "Why is my node unhealthy?" (#5428, #5396, #4849)

**Layout:** vertical list of gates, each green/red with WHY:

```
╭─ HEALTH ─────────────────────── prod-1 · 192.168.1.5:1633 · 2.7.1 ──╮
│ ✓ API reachable                 (3.2ms)                             │
│ ✓ Chain RPC                     block 8412930 · Δ +1 (1.2s)         │
│ ✓ Wallet funded                 0.092 ETH · stake 10 BZZ            │
│ ✓ Warmup complete               (started 3h12m ago)                 │
│ ✓ Peers                         87 connected · 12 inbound           │
│ ⚠ Bin saturation                bin 4 starving (3/8)                │
│ ✓ Reserve                       145,231 chunks · radius 8           │
│ ✗ Healthy for redistribution    storageRadius (8) < committed (9)   │
│   └─ wait for next 30-min reserve worker tick (in 12m)              │
│ ✓ Not frozen                    last frozen: never                  │
│ ✓ Sufficient funds to play      ~38 rounds runway                   │
╰──────────────────────────────────────────────────────────────────────╯
```

**Data sources:** `/health`, `/readiness`, `/wallet`, `/status`, `/redistributionstate`, `/chainstate`, `/topology` saturation (derived).

**Refresh:** 2s tick.

**Encoded knowledge:** the `storageRadius` decreases ONLY on the 30-min wakeup tick (`pkg/storer/storer.go:411`). Without this hint, operators stare at the gate forever wondering why nothing happens.

**Keys:** `↵` jump to relevant screen for any failing gate.

### S2. Stamps (volume+duration first)

**Anxiety:** depth/amount confusion (#4992 — community moving to retire), bucket collision (#3122, #4292)

**Per batch row:**

```
LABEL          VOLUME    USED   TTL        WORST BUCKET   STATUS    TOPUP
prod-mainnet   16 GB     34%    47d 12h    ▇▇▇▇▇▇▇░ 87%   ⚠ skewed   8.4 BZZ
batch-2        4 GB      12%    18d  3h    ▇▇░░░░░░ 22%   ✓         2.1 BZZ
batch-3        2 GB       0%    pending    -              ⏳ usable in 6 blocks (~72s)
```

**Bottom panel on focus** — bucket histogram of selected batch (`/stamps/{id}/buckets`):

```
Bucket fill (depth=22, bucketDepth=16, 65536 buckets, max=64 chunks each):
  worst:  64/64 (100%) ← uploads will FAIL on this bucket
  p99:    58/64
  p50:    18/64
  p01:     0/64
  ETA to bucket overflow: ~8 minutes at current rate
```

**Data sources:** `/stamps` @ 10s, `/stamps/{id}/buckets` on focus, `/chainstate` for usable countdown.

**Truth-telling:** `utilization` returned by API is `MaxBucketCount` not average (`pkg/postage/stampissuer.go:218-223`). Show BOTH "API utilization (worst bucket)" and "effective average". An immutable batch is dead when the worst bucket hits 100% even if average says 50%.

**Keys:** `b` buy new (volume + duration form), `t` topup, `d` dilute, `↵` bucket histogram, `c` copy batch ID.

### S3. SWAP / cheques

**Anxiety:** cashout confusion (#4663 + 8 dupes since 2021)

**Three tables:**

1. **Uncashed-IN** (cheques others sent us):

```
PEER (alias)              VALUE     CASHABLE NOW   GAS COST   PROFITABLE?
8a2f...c1   gateway-eu    0.082 ETH   YES          0.003 ETH  ✓ +0.079
b4c8...d7   storage-7     0.041 ETH   YES          0.003 ETH  ✓ +0.038
77a3...88   relay-uk      0.0008 ETH  no           0.003 ETH  ✗ wait
```

2. **Issued-OUT** (cheques we sent, drains our chequebook):

```
PEER          VALUE       LAST ISSUED   CHEQUEBOOK BALANCE
storage-3     0.018 ETH   2h ago        0.42 ETH (109 cheques runway)
```

3. **Refresh-vs-cheque split** (per peer, last 24h):

```
PEER          REFRESH-IN    REFRESH-OUT   CHEQUE-IN   CHEQUE-OUT
gateway-eu    142 BZZ       0             0.082 ETH   0
storage-3     12 BZZ        38 BZZ        0           0.018 ETH
```

**Data sources:** `/balances`, `/timesettlements`, `/settlements`, `/chequebook/balance`, `/chequebook/cheque`, `/wallet`, `/chainstate` (current gas price).

**Truth-telling:** computes "profitable to cash now?" using current gas price (from `/chainstate.currentPrice` or web3 estimate). Operators stop cashing 0.0008 ETH cheques when they see -0.002 ETH net.

**Keys:** `c` cashout focused row (with confirm), `C` cashout all profitable, `d` deposit, `w` withdraw, `f` filter to profitable only.

### S4. Lottery (redistribution rounds)

**Anxiety:** "Why am I not earning rewards?" (#4849)

**Top:** round timeline with current block tick:

```
Round 14528 · block 14528*152+118 = block-of-round 118/152
┌─ commit ──┬─ reveal ──┬─ claim ──┬─ sample ──┐
│ 0..38     │ 38..76    │ 76..152  │ ←→ next   │
│ ✓ done    │ ✓ done    │   ███▒░  │  pending  │
└───────────┴───────────┴──────────┴───────────┘
```

**Bottom:** last 20 rounds with phase outcomes + skip reasons:

```
ROUND   SELECTED  COMMIT  REVEAL  WIN  REWARD     REASON IF SKIPPED
14527   ✓         ✓       ✓       ✗   -          someone else won
14526   ✗         -       -       -   -          not selected (anchor depth too deep)
14525   ✓         ✗       -       -   -          INSUFFICIENT FUNDS to play
14524   ✓         ✓       ✓       ✓   142 BZZ    won
14523   ✓         ✗       -       -   -          FROZEN until 14530
...
```

**Side panel:** rchash benchmark — "your sampler took 18.3s last round; deadline is 38 blocks ≈ 95s — safe."

**Data sources:** `/redistributionstate` (round, phase, lastWonRound, lastFrozenRound, isFrozen, isHealthy, RoundData last 10), `/stake`, `/rchash` (on demand).

**Truth-telling:** the *reason* for each skip — research surfaced that `LastPlayedRound` is stale when frozen/skipped (`pkg/storageincentives/agent.go:286`). Reconstructing skip reasons from preconditions (`agent.go:394-447`) is what no other tool does.

**Keys:** `r` run rchash benchmark, `s` show stake details, `f` filter rounds.

### S5. Warmup (auto-shown when `isWarmingUp=true`)

**Anxiety:** 25–60 minute cold-start opacity (#4746)

```
WARMUP · 12m 38s elapsed
✓ batchstore sync           (chunks 142,000 / ~145,000)  98%
✓ postage snapshot loaded   (487 batches)
▒ peer bootstrap            (87 / target 100, bin 4 starving)
░ kademlia depth stable     (depth=8, fluctuating)
░ reserve fill              (0 / 65,536 chunks)
░ stabilization             (waiting on subscriber)
ETA: ~8 more minutes
```

**Data sources:** `/health`, `/status`, `/topology`, observed deltas.

**Refresh:** 1s tick.

### S6. Peers + bin saturation

**Top:** sortable peer table (configurable cols per `bee-tui.toml`):

```
PEER             ALIAS         BIN  PROXIMITY  SYNC RATE  RESERVE   REACHABLE
8a2f..c1          gateway-eu     8    8         142/s      45,000    in+out
b4c8..d7          storage-7      7    7          88/s      31,000    out only
...
```

**Bottom:** bin saturation strip:

```
BIN   POPULATION   TARGET   STATUS
0     ████████████████ 18    8/18    ✓ over
...
4     ███             3      8/18    ⚠ STARVING
...
```

**Truth-telling:** per-bin starvation isn't visible in `/status/peers`; derive from `/topology` populations vs `SaturationPeers=8`/`OverSaturationPeers=18` constants (`pkg/topology/kademlia/kademlia.go:54-55`).

**Keys:** `↵` drill peer (balance, settlements, cheque, kademlia bin), `x` disconnect, `b` blocklist, `p` ping.

### S7. Network / NAT

**Anxiety:** "I have peers but I'm unreachable" (#4194)

```
PUBLIC ADDRS (advertised):
  /ip4/77.x.x.x/tcp/1634/p2p/16Uiu...
  /ip6/2a01:.../tcp/1634/p2p/16Uiu...

INBOUND CONNECTIONS:    12 (last 5min: 3 new)
OUTBOUND CONNECTIONS:   75
NAT TYPE (AutoNAT):     full-cone (stable for 9m)
PORT-CHECK:             tcp/1634 reachable from observer ✓
RELAY CANDIDATES:       2 known
```

**Truth-telling:** `isReachable` flickers under symmetric NAT — show stability over 10 min, not snapshot. If flickering, suggest port-forward.

### S8. RPC health

```
RPC ENDPOINT:    https://rpc.gnosischain.example
LATENCY:         142ms p50 · 380ms p99 (last 100 calls)
ERROR RATE:      0.4% (last 1h)
BLOCK HEIGHT:    8412930 · Δ +1 vs network · 1.2s ago
PENDING TX:      2 stuck > 2min — [view]
```

**Truth-telling:** RPC fragility blamed on Bee but not Bee's fault. Show this prominently.

### S9. Tags + uploads

Active tags table with split/sent/synced live progress (WS `/chunks/stream` feed). Per-tag drill shows seen/stored/sent/synced delta over time.

### S10. Command log (always available, `^L` to toggle)

lazygit-style append-only HTTP req/res tail:

```
14:32:18  GET  /status                       200  3ms
14:32:20  GET  /stamps                       200  18ms
14:32:21  POST /stamps/topup/abc.../1000     200  2.1s
14:32:23  WS   /chunks/stream                — connected
```

This is the trust anchor + live tutorial. Operators learn the API by watching it.

### Common UI

**Top bar:** profile name • bee version • API latency • pending writes • clock
**Bottom bar:** dynamic keymap legend (lazygit pattern — changes per focus)
**Command bar:** `:` opens k9s-style — `:stamps`, `:peers /reachable`, `:health`, `:quit`, `:diagnose` (export bundle)
**Help overlay:** `?` shows screen-specific keymap
**Diagnostic bundle export:** `:diagnose` produces a redacted Discord-paste-ready snippet with health gates state + version + last 50 commands

---

## 9. Config

`~/.config/bee-tui/config.toml`:

```toml
[ui]
theme = "default"               # default | mono | high-contrast
ascii_fallback = false
fps_cap = 60
tick_ms = 250

[refresh]                       # tig-style per-resource policy
status      = "periodic:2s"
stamps      = "periodic:10s"
buckets     = "manual"
chequebook  = "periodic:30s"
tags        = "periodic:5s"
chunks      = "stream"

[[nodes]]
name    = "prod-1"
url     = "http://10.0.1.5:1633"
token   = "@env:BEE_TOKEN_PROD1"
default = true

[columns.peers]                 # lntop-style configurable columns
fields = ["overlay", "alias", "bin", "proximity", "sync_rate", "reserve", "reachable"]

[keys]                          # tig-style layered overrides
quit = "q"
help = "?"
command = ":"
diagnostic_bundle = "ctrl+d"
```

---

## 10. Testing

| Layer | Tool | Scope |
|---|---|---|
| Unit | `cargo test` | `update()` per `Action`, derived-state computation |
| View snapshot | `insta` + `Buffer::pretty_print` | Each component at fixed sizes (80×24, 132×40, 200×60) |
| API integration | `wiremock` | Mock Bee responses for every endpoint; assert state transitions |
| Live integration | bee-rs Sepolia rig | Reuse existing per memory (BEE_BATCH_ID env); behind `--features sepolia-tests` |
| Binary smoke | `assert_cmd` | `--version`, `--check-config`, `--diagnostic-bundle` |
| Visual regression | VHS `.tape` files | Screen recordings → diffable text golden output |

**CI matrix:** ubuntu-latest, macos-latest, windows-latest × stable, 1.85 (MSRV). `cargo-deny`, `cargo-audit`, `clippy --all-targets -- -D warnings`.

---

## 11. Release & distribution

- `cargo-dist` for installers + prebuilt archives (Linux x86_64-musl + aarch64-musl, macOS universal, Windows x86_64)
- `cargo binstall bee-tui` for Rust-toolchain users
- Homebrew tap, Scoop, AUR, MSI
- Cosign-signed releases (matches bee-go/bee-rs posture)
- `lto = "fat"`, `codegen-units = 1`, `strip = true` — target binary <8 MB
- mdBook docs site with VHS-recorded GIF demos
- `awesome-swarm` PR at v1.0
- Optional: Ratzilla web preview at `bee-tui.dev/demo` (same code, WASM build)

---

## 12. Roadmap

| Version | Scope | Effort |
|---|---|---|
| **Validation gate** | Discord post + 5 operator interviews | 1 week |
| **bee-rs 1.3.0** | 6 prereqs (WS upload + 2 endpoints + ergonomics) | 1 week |
| **bee-tui v0.1** | S1 Health, S2 Stamps, S10 Command log; single-node; CI; insta tests | 2 weeks |
| **bee-tui v0.2** | S3 SWAP, S4 Lottery + rchash benchmark | 1.5 weeks |
| **bee-tui v0.3** | S5 Warmup, S6 Peers, S7 NAT, S8 RPC | 1.5 weeks |
| **bee-tui v0.4** | Multi-node config + `:context` switcher + diagnostic bundle | 1 week |
| **bee-tui v0.5** | S9 Uploads + WS chunks/stream; theme system | 1 week |
| **bee-tui v0.9** | Polish, mdBook, VHS demos, cargo-dist setup, beta with the 50–200 | 1 week |
| **bee-tui v1.0** | Stable; awesome-swarm PR; blog post | release-only |

**Total: ~10 weeks** from green-light to v1.0, including prereqs and validation. Without prereqs/validation: ~8 weeks of pure bee-tui work.

---

## 13. Risks (sharpened)

| Risk | Likelihood | Mitigation |
|---|---|---|
| **Niche too small to matter** (lntop-style fade) | Medium | Validation gate; pivot to Grafana pack if signal weak |
| **Bee API drift between minor versions** | High | Pin bee-rs minor; show API version in header; warn-banner on mismatch |
| **WS reconnect storms on flaky home networks** | Medium | Exp backoff, max 5 retries, manual retry banner |
| **30–60s feed lookups freeze a screen** | High | Per-screen `CancellationToken` child, throbber, request-in-flight indicator |
| **Logging stdout corrupting alt-screen** | Certain if mishandled | `tracing-appender` non-blocking to file; never stdout |
| **Operators on Windows Terminal with broken Unicode** | Medium | `--ascii` flag; fallback box-drawing |
| **Token leaks in logs / bundles** | Severe if leaked | Redaction layer; never log full URL with auth; bundle export sanitizes |
| **Author burnout (single-maintainer death)** | High historically | mdBook contributing guide; component template structure makes onboarding cheap |
| **Pushsync silent chunk loss (#5400)** masquerades as TUI bug | Medium | Surface stewardship status per ref; banner if Bee version has known issues |

---

## 14. Open decisions

1. **Repo location:** `github.com/ethswarm-tools/bee-tui` (matches bee-go, bee-bench)?
2. **Crate name reservation:** `bee-tui` — reserve via dummy publish before announcing?
3. **License:** match bee-rs (MIT or Apache-2.0)?
4. **Bee version target:** `2.7.x` API version 8.0.0 — drop older?
5. **Validation outcome:** if Discord signal is weak, build the **Grafana dashboard pack** instead. Are you open to that pivot?
6. **Multi-node priority:** scoped to v0.4; if a multi-node setup is the killer use case, swap to v0.1.
7. **PSS/GSOC console:** cut to v0.5; is this a marketing-screenshots feature wanted earlier?

---

## 15. Concrete next step

If this v2 plan looks right, the right next move is **Step 1 of the Validation Gate** — post the screen mockups to `#node-operators` Discord. Drafting:
- the post text + screenshot mockup descriptions
- the 5-question template
- a simple Google Form / Markdown reply template

Or if validation is skipped (every operator pain point above has GitHub issue evidence with reactions), the right next move is **bee-rs v1.3.0** (1 week of work to close the 6 prereqs).

---

## Research index

- [`research/01-bee-feature-surface.md`](research/01-bee-feature-surface.md) — Bee API endpoint inventory and TUI-shaped data sources
- [`research/02-tui-design-patterns.md`](research/02-tui-design-patterns.md) — Design patterns from k9s, lazygit, btop, gh dash, dive, bottom
- [`research/03-bee-rs-audit.md`](research/03-bee-rs-audit.md) — bee-rs v1.2.0 endpoint audit + 6 prereq gaps
- [`research/04-production-ratatui-patterns.md`](research/04-production-ratatui-patterns.md) — Architecture patterns from gitui, atuin, bottom, gpg-tui
- [`research/05-operator-pain-points.md`](research/05-operator-pain-points.md) — Field research: top 10 Bee operator pain points with GitHub issue evidence
- [`research/06-bee-internals.md`](research/06-bee-internals.md) — Deep dive into Bee subsystem internals
- [`research/07-competitor-cockpits.md`](research/07-competitor-cockpits.md) — Decentralized-storage TUI prior art
- [`research/08-ratatui-2026-sota.md`](research/08-ratatui-2026-sota.md) — Current 2026 Ratatui state-of-the-art
