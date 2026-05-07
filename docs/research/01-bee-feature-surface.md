# Bee TUI feasibility — API surface, live data, and killer views

## 1. API surface (from `/home/calin/work/swarm/bee-apis/bee/openapi/Swarm.yaml`)

Bee's API is **unified** (no separate debug port since v2.x) — all 67 endpoints live in one spec. Grouped:

- **Node info / status**: `/health`, `/readiness`, `/node`, `/addresses`, `/welcome-message`, `/status`, `/status/peers`, `/status/neighborhoods`, `/wallet`
- **Connectivity**: `/peers`, `/peers/{address}`, `/blocklist`, `/topology`, `/connect/{multiAddress}`, `/pingpong/{address}`
- **Chain / reserve**: `/chainstate`, `/reservestate`, `/redistributionstate`, `/transactions`, `/transactions/{txHash}`
- **Postage stamps**: `/stamps`, `/stamps/{batch_id}`, `/stamps/{batch_id}/buckets`, `/stamps/{amount}/{depth}` (POST), `/stamps/topup/...`, `/stamps/dilute/...`, `/batches`
- **Accounting / cheques / settlements**: `/accounting`, `/balances`, `/balances/{address}`, `/consumed`, `/consumed/{address}`, `/chequebook/{balance,address,cheque,cheque/{peer},cashout/{peer},deposit,withdraw}`, `/settlements`, `/settlements/{address}`, `/timesettlements`
- **Stake / rewards**: `/stake`, `/stake/{amount}`, `/stake/withdrawable`, `/rchash/{depth}/{a1}/{a2}`
- **Data I/O**: `/bytes`, `/bytes/{ref}`, `/chunks`, `/chunks/stream` (WS), `/chunks/{address}`, `/bzz`, `/bzz/{ref}`, `/bzz/{ref}/{path}`, `/feeds/{owner}/{topic}`, `/soc/{owner}/{id}`, `/envelope/{address}`
- **Tags / pinning / stewardship**: `/tags`, `/tags/{uid}`, `/pins`, `/pins/{ref}`, `/pins/check`, `/stewardship/{ref}`
- **Pub/sub (WebSocket)**: `/pss/send/{topic}/{targets}`, `/pss/subscribe/{topic}` (WS), `/gsoc/subscribe/{address}` (WS) — confirmed in `/home/calin/work/swarm/bee-apis/bee-go/pkg/api/websockets.go:31,47`
- **Ops / debug**: `/loggers`, `/loggers/{exp}`, `/grantee`, `/grantee/{ref}`

## 2. Live / streaming candidates (poll on a live screen)

- **`/status`** (`SwarmCommon.yaml:879`) — gold mine: `connectedPeers`, `pullsyncRate`, `reserveSize`, `reserveSizeWithinRadius`, `storageRadius`, `committedDepth`, `lastSyncedBlock`, `isWarmingUp`, `isReachable`, `proximity`. Single endpoint covers most of the dashboard.
- **`/status/peers`** — same fields per connected peer; ideal for a peer-comparison table.
- **`/status/neighborhoods`** — reserve density per neighborhood.
- **`/chainstate`** — `block` vs `chainTip` (sync delta), `currentPrice` (per-chunk cost).
- **`/peers`** + **`/topology`** — connection churn, depth, kademlia bins.
- **`/tags/{uid}`** — `split / seen / stored / sent / synced` counters; perfect progress bar source for in-flight uploads.
- **`/stamps`** — `utilization`, `batchTTL` (countdown), `usable` (chain-confirmation flag).
- **`/stamps/{batch_id}/buckets`** — per-bucket collisions; surfaces stamp-exhaustion hotspots before TTL says so.
- **`/accounting`** + **`/balances`** — drift, surplus, ghost balances per peer.
- **`/settlements`** / **`/chequebook/cheque`** — last cheque per peer; live cheque arrival.
- **`/transactions`** — pending tx queue (stuck cashouts, batch buys).
- **WebSocket streams**: `/chunks/stream` (uploader), `/pss/subscribe/{topic}`, `/gsoc/subscribe/{address}` — true push, no polling.

## 3. Operational pain TUI solves

- **Bootstrap watch**: poll `/status` + `/topology` + `/peers` once/sec until `connectedPeers` stabilises and `isWarmingUp` flips false. Today: tail logs + curl loops.
- **Stamp lifecycle**: `batchTTL` countdown beside `utilization` and bucket histogram in one frame; one-key topup/dilute via `/stamps/topup` / `/stamps/dilute`. Web dashboard shows numbers but no watch mode.
- **Misbehaving peer drilldown**: enter on a row in `/status/peers`, get its `/balances/{address}`, `/settlements/{address}`, `/chequebook/cheque/{peer}`, `/pingpong/{address}`, kademlia bin from `/topology`. Hot key to disconnect (`DELETE /peers/{address}`) or blocklist.
- **Upload sync progress**: WS `/chunks/stream` + `/tags/{uid}` deltas; today users curl-loop tags.
- **Stuck-chunk debug**: `/chunks/{address}` HEAD across the topology; `/stewardship/{ref}` re-upload.
- **Redistribution / staking**: `/redistributionstate`, `/stake`, `/rchash` — operators care about whether their node won the round.

## 4. NOT a fit

- Big-file uploads (`/bzz`, `/bytes` POST) — blocking, want streamed CLI.
- Key/identity generation, mnemonic entry — one-shot, security-sensitive.
- Manifest authoring / ACT grantee list editing — better as forms/CLI.
- Long-form download to disk — pipe to a file, not a TUI.
- Static config (`bee.yaml`) editing — file editor.

## 5. Multi-node angle — yes, strong

`swarm-cli` (`/home/calin/work/swarm/bee-apis/swarm-cli/src/config.ts`) only supports a single active context. **Beekeeper** (`/home/calin/work/swarm/dev/beekeeper`) is the real precedent: orchestrates clusters of Bee nodes (k8s) and would benefit hugely from a k9s-style switcher. Operators routinely run gateway + storage + light nodes; comparing `pullsyncRate`, `reserveSize`, `batchTTL` across them in a single sortable table is the use case.

## 6. Five killer views

1. **Cluster overview** (header view): rows = nodes, columns = `connectedPeers`, `pullsyncRate`, `storageRadius`, `reserveSize`, `block/chainTip Δ`, `isReachable`, `isWarmingUp`. Source: `/status` + `/chainstate` per node. Hot keys: enter to drill, `s` to sort.
2. **Stamp manager**: per-batch row with TTL countdown bar, utilization %, depth, bucket-collision sparkline. Source: `/stamps` + `/stamps/{id}/buckets`. Actions: `t` topup, `d` dilute, `b` buy.
3. **Peer inspector**: split-pane — top: `/status/peers` table sorted by proximity; bottom: drill view with `/balances/{addr}`, `/settlements/{addr}`, `/chequebook/cheque/{peer}`, kademlia bin. Hot key `x` to disconnect, `p` to pingpong.
4. **Live upload tracker**: rows = active tags; columns = `split / sent / synced` progress bars + ETA. Source: `/tags` + WS `/chunks/stream`. Cancel/pin actions.
5. **Neighborhood / redistribution heatmap**: `/status/neighborhoods` reserve density vs `/redistributionstate` (round, isFrozen, lastWonRound). Storage-node operators stare at this for SWARM rewards.

Bonus: **PSS/GSOC console** — a chat-like view subscribed to `/pss/subscribe/{topic}` and `/gsoc/subscribe/{address}`; the WS endpoints make this trivial and there is no good existing UI.

**Verdict**: strong fit. `/status`, `/status/peers`, `/status/neighborhoods`, `/tags/{uid}`, plus the two WS subscribe endpoints alone justify the project; multi-node + stamp lifecycle + peer drilldown push it from "useful" to "killer."
