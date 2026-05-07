# Bee-TUI Prior Art Survey — Decentralized Storage & P2P Network Cockpits

Survey research: do TUIs / interactive terminal cockpits exist in adjacent decentralized-storage and p2p-network ecosystems?

## A. TUI Inventory Table

| Network | Native TUI? | Project | Stack | Stars / Last commit | Status |
|---|---|---|---|---|---|
| **IPFS (kubo)** | Marginal | [jamesthesken/ipfs-tui](https://github.com/jamesthesken/ipfs-tui) | Go + Bubble Tea | 20 / 2022-07 | Dead, file-browser only — never reached node-ops cockpit |
| **IPFS Cluster** | No | — | — | — | Only `ipfs-cluster-ctl` CLI + Prometheus/Grafana |
| **Filecoin** | No (web only) | [Curio](https://github.com/filecoin-project/curio) ships a built-in **web** GUI | Go + web | active | Explicit choice: web dashboard, not TUI |
| **Storj** | No real TUI | [storj/awesome-storj](https://github.com/storj/awesome-storj) lists a few one-shot terminal scripts; official `dashboard-cli` is a one-shot printout | bash/python scripts | scattered | "Terminal dashboard" forum thread = a print-once script |
| **Arweave** | No | — | — | — | All Grafana / web-gateway dashboards |
| **Iroh** | No (CLI only) | `iroh doctor` is a diagnostics CLI | Rust | active | No cockpit. Very small surface area on purpose. |
| **Bitcoin Lightning (LND)** | **Yes — strongest analog** | [edouardparis/lntop](https://github.com/edouardparis/lntop) | Go + gocui | **190 / archived 2025-09** | Archived; fork [hieblmi/lntop](https://github.com/hieblmi/lntop) carries on |
| **Lightning (LND, alt)** | Dead | [LLeny/lncli-curses](https://github.com/LLeny/lncli-curses) | Go + gocui | 7 / 2019-02 | Abandoned |
| **Lightning (CLN)** | No | `summary` plugin prints once | — | — | No interactive TUI |
| **Bitcoin/LN (web peers)** | n/a | ThunderHub, RTL | TypeScript/React | active | Web, not TUI |
| **Eth EL (Reth/Geth/Erigon)** | No | — | — | — | All Grafana |
| **Eth CL (Lighthouse/Prysm/Teku)** | No | — | — | — | All Grafana / beaconcha.in |
| **Eth staking installer** | Bash-TUI | [coincashew/EthPillar](https://github.com/coincashew/EthPillar) | bash + `whiptail` | 71 / 2026-04 | Alive, but installer-style menus, not live cockpit |

**Reference TUIs studied (not network-specific):** [k9s](https://github.com/derailed/k9s) (33.5k, 2026-04), [tig](https://github.com/jonas/tig) (13.2k, 2026-05), [lazygit](https://github.com/jesseduffield/lazygit) (77.5k, 2026-05), [gitui](https://github.com/gitui-org/gitui) (21.9k, 2026-04).

## B. Top 5 Design Ideas to Steal

1. **k9s `:command` mode for resource navigation.** Source: [internal/view/command.go](https://github.com/derailed/k9s/blob/master/internal/view/command.go) — k9s parses `:pod ns-x /filter label=foo` via `cmd.Interpreter` returning `NSArg`, `FilterArg`, `LabelsSelector`. Bee map: `:peers /reachable`, `:stamps depth=20`, `:chunks ref=<addr>`. One mental model that scales without screen-locked menus. The dispatcher is "special commands first, then resource alias resolve" — copy that pattern.

2. **k9s watch/informer separation (`internal/watch` distinct from `internal/view`).** The model fans out k8s API watch streams; views subscribe. For Bee this is critical: `/chunks` POST events, peer connect/disconnect, cheque updates and stamp TTL ticks all need long-lived streams. Don't poll from views; have one watch layer feed an event bus. Bee has no native long-poll API, so the watch layer also handles **polling cadence per resource type** (peers slow, balances medium, queue fast).

3. **lazygit's "command log" pane.** Source: [Lazygit-5-Years-On post](https://jesseduffield.com/Lazygit-5-Years-On/) — every TUI action shows the underlying git command. For Bee: show the actual HTTP request/response (`POST /chunks?batch=...` → `201`). Operators trust the tool because it's transparent; also doubles as a tutorial for the API. Skeptic note: this is the *most-praised* lazygit feature in HN threads.

4. **tig's keymap layering: view-specific → generic → built-in.** Source: [tigrc(5)](https://jonas.github.io/tig/doc/tigrc.5.html). And tig's `refresh-mode` (`auto` / `periodic` / `manual` / `after-command`) — per-view refresh policy. For Bee, a node-status view wants 2s ticks; a chunk-explorer wants manual; the queue wants stream. Don't hardcode one global refresh.

5. **lntop's columnar customization** ([README](https://github.com/edouardparis/lntop)) — its `[views.channels]` config lets ops pick which columns matter (capacity, fees, alias, last-update). Bee operators care about wildly different things (gateway op vs storage incentivized vs dev). Make the peer/stamp tables column-configurable from a `bee-tui.toml`.

## C. Top 3 Anti-patterns Observed

1. **Bubble Tea-style "one giant `Update` switch on `tea.Msg`" doesn't scale to live-streamed multi-resource state.** ipfs-tui shows the symptom: 17 commits, never grew past file browse. Bubble Tea's `Cmd`-based async is great for forms and wizards; it's awkward for "10 concurrent watch goroutines feeding 5 viewports." k9s avoided this by *not* using Bubble Tea — it uses tview/tcell with explicit goroutines. **For Bee, choose the framework based on long-running streams, not screenshot aesthetics.**

2. **One-shot "dashboard" scripts that print and exit** (Storj `dashboard-cli`, CLN `summary` plugin). They're trivial to write and operators briefly love them, but you lose any state between invocations and can't show events. Skip this temptation; if you're going to make a TUI, make it persistent.

3. **Bash `whiptail` installer-flavor TUIs** (EthPillar). Fine for setup wizards, terrible for ongoing ops because every action is modal and you can't see live state. Bee-TUI should *not* be a launcher — operators need to *watch* peers/queue/cheques over time.

Bonus: **lncli-curses being abandoned (2019, 7 stars) while lntop reached 190 before archiving** (2025) tells you the market: the scope wasn't "wrap every CLI command" but "watch the few signals that matter."

## D. The Lightning Network Analogy — Deeper

The mapping is real and tighter than expected:

| Lightning concept | Bee equivalent |
|---|---|
| Channel balance (local/remote) | Cheque balance (sent/received per peer) |
| Channel capacity | Stamp batch capacity remaining |
| Fee rate (base + ppm) | Chunk price / postage stamp price |
| Forwarding events | Forwarded chunk requests / retrievals |
| Peer gossip | Kademlia peer discovery |
| HTLC in-flight | Chunk push in-flight in pusher queue |

**[lntop](https://github.com/edouardparis/lntop) is the single closest analog**, and it's instructive that it *was archived in 2025* with only 190 stars despite the Lightning ecosystem being huge. Why? Because **most LN operators eventually moved to web (ThunderHub) or hosted services (Umbrel/Start9)**. The lesson is not "build a TUI like lntop"; it's "lntop was right about *what* to display (channels table + routing event tail + fee config) and wrong about assuming TUIs alone would suffice." Bee-TUI should:

- **Steal lntop's three-pane layout**: list of peers/channels (top), event tail (middle, like its routing view — append-only stream of forwards/cheques), summary status bar (bottom).
- **Steal its config-driven columns** (above).
- **Avoid its biggest hole**: lntop is read-mostly; users complained they still needed `lncli` for actions. Bee-TUI must let you do `:stamps buy`, `:peers connect`, `:dev fund` from inside.
- **Read the [hieblmi/lntop fork](https://github.com/hieblmi/lntop)** before designing — it shows what 2024-era LN ops actually wanted that the original missed.

## E. Why This Niche Is Empty — Honest Read

It's mostly **structural with one real opportunity**:

1. **Decentralized-storage operators are a tiny audience** vs k8s/git users. k9s has ~1M k8s admins to serve; a Bee-TUI has thousands. Authors don't get a return on the effort; ipfs-tui (20 stars) and lncli-curses (7 stars) died for this reason.

2. **The "operate one node from a laptop" model lost.** Most serious storage/blockchain ops run fleets behind Prometheus + Grafana + alertmanager. A TUI helps **one** node, which is exactly the use case the industry de-emphasizes. Filecoin Curio went **explicitly web** for this reason.

3. **Most networks have already invested in web dashboards** (ipfs-webui, Storj operator dashboard, ThunderHub, Curio GUI). A web UI works on phones, screen-shares cleanly, and reuses front-end skills. TUI authors compete with that on aesthetics alone.

4. **The real opportunity for Bee specifically:** Bee operators are *developer-leaning* (running gateways, debugging stamp/cheque mechanics, exploring chunks/feeds), not just passive stakers. That's the same audience that adopted k9s and lazygit. **The niche is empty, but it's empty because nobody has tried the k9s playbook on a Web3 node** — every prior attempt was either a viewer (lntop, ipfs-tui) or an installer (EthPillar). A k9s-shaped Bee-TUI with `:command` navigation, watch-stream model layer, and real write actions would be the first of its kind.

**Skeptical caveat:** before committing, validate by asking 5 actual Bee operators if they'd run this in tmux daily. If most reply "I just curl the API and check Grafana," the structural answer wins and you should ship a Grafana dashboard pack instead.

**Stack recommendation given findings:** Rust + Ratatui. Reasons: (a) Bee's debug API surface is not enormous — you don't need lazygit-scale ergonomics; (b) Ratatui's explicit-render model fits k9s-style watch streams better than Bubble Tea's `tea.Msg` switch (see anti-pattern #1); (c) the Bee Rust client (`bee-rs`) gives you a typed in-process client; (d) gitui's vision statement ("only show shortcuts applicable in the current situation in a quick-bar at the bottom") is the right ergonomic for operators who don't want to memorize keymaps.

## Sources

- [jamesthesken/ipfs-tui](https://github.com/jamesthesken/ipfs-tui)
- [edouardparis/lntop](https://github.com/edouardparis/lntop) (archived) and successor [hieblmi/lntop](https://github.com/hieblmi/lntop)
- [LLeny/lncli-curses](https://github.com/LLeny/lncli-curses)
- [derailed/k9s](https://github.com/derailed/k9s) and [internal/view/command.go](https://github.com/derailed/k9s/blob/master/internal/view/command.go)
- [jonas/tig](https://github.com/jonas/tig) and [tigrc(5) docs](https://jonas.github.io/tig/doc/tigrc.5.html)
- [jesseduffield/lazygit](https://github.com/jesseduffield/lazygit) and [Lazygit Turns 5](https://jesseduffield.com/Lazygit-5-Years-On/)
- [gitui-org/gitui](https://github.com/gitui-org/gitui)
- [coincashew/EthPillar](https://github.com/coincashew/EthPillar)
- [filecoin-project/curio](https://github.com/filecoin-project/curio)
- [storj/awesome-storj](https://github.com/storj/awesome-storj)
- [HN: lazygit vs tig vs gitui](https://news.ycombinator.com/item?id=30706467)
