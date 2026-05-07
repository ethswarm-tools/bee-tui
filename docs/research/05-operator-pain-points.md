# Bee Operator Pain Points — Field Research

Sources: GitHub issues across 5 ethersphere repos, docs.ethswarm.org FAQ + Staking + Connectivity pages, blog.ethswarm.org 2025/2026 release notes, awesome-swarm registry, Beest + Doctor Bee READMEs.

## A. Top 10 Operator Pain Points (severity × frequency)

1. **Postage stamp depth/amount is opaque and easy to misuse** — "depth+amount" forces operators to reason about chunk math instead of "how much data, for how long". Issue #4992 (2025-02, OPEN, 8 comments incl. core devs proposing volume+duration as default; mfw78 suggests retiring depth from docs entirely). Reinforced by closed dupes #3092, #3122, #4292, #3320, #3193, #3136, #418 (swarm-cli "show current depth"), #479 ("include effective volume"). **Frequency: extreme.** TUI fix: **YES** — Stamps screen shows volume + TTL by default, with depth/amount as advanced; built-in dilute/topup wizard.

2. **"Why is my stamp suddenly 100% utilized?" (bucket collision)** — operators don't realize a single saturated bucket marks the whole batch unusable. Issue #4292 (1.17.3 user reports yesterday-bought stamp at 100% on one small upload), and #3122 ("postage stamps with depth < 20 do not report accurate utilization… confusing for people who just try the minimum"). istae confirmed it as "the utilization problem is a known one". **Frequency: high.** TUI fix: **PARTIAL** — show worst-bucket fill alongside aggregate utilization, plus a clear "this batch will fail before it shows 100%" warning when worst-bucket >> mean.

3. **Cheque cashout confusion (cheque vs balance vs settlement)** — operators think their node should auto-cash incoming cheques and panic when balance "doesn't move". Issue #4663 (5 comments, ldeffenb explains "it is up to the cheque beneficiary…"), historical dupes #1787, #1816, #1297, #971, #1945, #2057, #2089, #2075. swarm-cli #499 ("withdraw all funds"), #684 ("hard to specify BZZ amount"). FAQ explicitly has a section "what is the difference between totalBalance and availableBalance". **Frequency: very high (recurring for years).** TUI fix: **YES** — dedicated SWAP screen showing per-peer uncashed/cashable amounts, gas-cost-aware "would-cash-out" hints, and one-keypress cashout.

4. **"My node is unhealthy — why?" (storage radius / neighborhood mismatch)** — opaque health gating from redistribution. Issue #5428 (2026-04, OPEN — node stuck `isHealthy=false`, radius 10 vs neighborhood 9, redistribution never resumes), #5396 (2026-03, "storageRadius decrease logic too restrictive on active networks"), #4697, #5333. **Frequency: rising as redistribution adoption grows.** TUI fix: **YES** — Health screen showing every gate (isFullySynced, hasSufficientFunds, isFrozen, isHealthy, committedDepth vs storageRadius vs neighborhoodSize) with green/red and the *reason* each gate is failing.

5. **"Why am I not winning rewards?" — frozen, slow sampler, sparse vs crowded neighborhood** — sampler timeout on weak hardware silently kills earnings. Issue #4849 (OPEN, 4 comments — 0xCardiE: "should be in docs with some highest warning on staking part"; ldeffenb: rchash itself is heavy so cannot run periodically). Docs explicitly call this out as the failure mode. **Frequency: high — central to staking thesis.** TUI fix: **YES** — Earnings/Lottery screen with rchash benchmark button, last 20 rounds (commit/reveal/claim per round), neighborhood density vs your stake density, and frozen-status reason.

6. **Long warmup / readiness opacity** — node looks dead for 5–60 minutes. Issue #4746 (OPEN, 6 comments — "25–60 minutes with several rpc endpoints"), #4894 (closed, "non-synced reserve hashed, committed, revealed — warmup timing"), #5413 ("Warmup complete signal arrives twice"). bee-dashboard #693 ("Detect warmup period"). May 2025 dev update brags about "spin-up time to under 5 minutes" — confirmation it was a top complaint. **Frequency: every cold start.** TUI fix: **YES** — first-launch screen with phased progress (batchstore sync, postage snapshot, peer bootstrap, kademlia depth, reserve fill).

7. **RPC fragility and lack of failover** — the #1 cause of "syncing in progress forever" (#4941, #4560 "block range is too wide"), of stuck pending tx (#5123, #5129, #3443), of cancellation surprises. Closed but persistent: #4419 (7 comments, "support multiple RPC endpoints"), #4335, #4131 ("excessive RPC calls"), #5388, #5386, #5441 (env-var bug), #5404. **Frequency: very high.** TUI fix: **PARTIAL** — surface block-height delta, recent RPC error rate, and pending-tx queue; cannot fix the RPC itself, but can stop operators blaming Bee.

8. **Reachability / NAT contradicts /topology** — issue #4194 (OPEN, 3 comments — `/status` says unreachable while `/topology` shows inbound connection; user has IPv6 but lousy IPv4). #5355 (DCUtR for NAT'd peers), #5380 (aggressive peer burst at startup looks like port scanning to ISPs). FAQ has a whole section "Why are there sometimes 300+ peers and sometimes 30?". **Frequency: medium-high (every home operator).** TUI fix: **YES** — Network screen showing public addrs advertised, inbound count, NAT type, port-check, AutoTLS status; explicit "you only have outbound, here's what to do".

9. **Pinning large files via /chunks is broken** — #4864 (OPEN, 7 comments — "even using tags to upload with sessions, is not possible to pin large files"). #5096 ("tagging does not work on direct BZZ upload"). swarm-cli #540 ("show upload progress"), bee-js #525 (10 comments "include content length for progress"), #650 ("support waiting on postage stamp to be usable"). **Frequency: hits everyone uploading >some-MB.** TUI fix: **PARTIAL** — Tags + Pins screen with synced/total chunk progress; we can't fix the underlying bug.

10. **Migration / upgrade fear** — issue #5216 (OPEN, "Migration step 06 fails… causing total data loss" with recommendation to "nuke"), #5285 ("version life cycle expectancy is too short"), #4663 dupes, swarm-desktop #438 ("Bee start: unknown parameters"), #514 ("new desktop still installs old bee"). **Frequency: every release.** TUI fix: **PARTIAL** — show running vs latest version, migration path warnings; surface backup/snapshot prompts.

## B. Misunderstood Mechanics

- **Depth vs amount.** Misconception: "depth = how long". Reality: depth = chunk capacity (2^depth slots × 4KB), amount = price-per-chunk that funds TTL. TUI: present "size + duration" sliders; show derived depth/amount in a collapsed details pane only.
- **Effective vs theoretical volume + bucket collision.** Misconception: "I can fill 100% of a 16GB stamp". Reality: bucket overflow stops uploads at ~50% theoretical for low depths (#3122). TUI: show effective volume by default (#479 was a 2-year request for swarm-cli); show worst-bucket fill, with a red bar when worst-bucket > 2× mean.
- **Mutable vs immutable batch.** Misconception: mutable = "lasts forever". Reality: mutable overwrites oldest chunks once full (issue #5334 calls out misleading source comment). TUI: badge each batch + tooltip explaining overwrite behavior.
- **Neighborhood selection.** Misconception: "more peers in my neighborhood = more rewards". Reality: opposite — sparse neighborhoods win (docs.ethswarm.org/staking). #4991 OPEN "Auto-Neighborhood Balancing". swarm-cli #596 "Add neighborhood doubling related info / warnings". TUI: Neighborhood screen with population vs network avg + Swarmscan-suggested neighborhoods.
- **SWAP cheques vs settlements vs balance.** Misconception: "my chequebook should be filling up". Reality: incoming cheques live as IOUs until *you* cash them; outgoing cheques drain available balance immediately. TUI: separate "uncashed-IN" and "issued-OUT" columns.
- **Warmup.** Misconception: "node started but isn't working". Reality: batchstore + reserve hash phases are mandatory; participation has 2-round delay after stake/neighborhood change (docs/staking). TUI: phased progress + countdown.
- **Reachability.** Misconception: "I have peers therefore I'm reachable". Reality: outbound-only is fine for retrieval but won't earn full incentive credit (docs/connectivity, #4194). TUI: explicit "inbound count = 0 → port-forward needed".

## C. "Why is my node X" — top 5 anxieties

| Anxiety | API data | TUI screen |
|---|---|---|
| Why is my balance dropping? | `/chequebook/balance` (total vs available), `/settlements`, `/chequebook/cheque` per peer, `/wallet`, `/transactions` | SWAP/Wallet — totalBalance vs availableBalance, last N outgoing cheques, gas spent on cashouts |
| Why am I not earning rewards? | `/redistributionstate` (lastPlayedRound, lastFrozenRound, isFrozen, isHealthy), `/status` (committedDepth, storageRadius, neighborhoodSize, reserveSize), `/stake` | Lottery — last 20 rounds with phase outcome; rchash benchmark button |
| Why am I unhealthy / not warming up? | `/health`, `/readiness`, `/status` (isFullySynced, pullsyncRate, batch sync), `/chainstate` | Health gates — phased warmup; each gate green/red with cause |
| Why does my batch say "not yet usable"? | `/stamps/{id}` (`usable`, `blockNumber`), `/chainstate` (currentBlock) | Stamps — countdown blocks until usable, ETA in seconds |
| Why is my chequebook empty? | `/chequebook/cheque` (per peer last received), `/settlements`, `/chequebook/cashout/{peer}` | SWAP — uncashed-IN list; one-key cashout; gas-vs-amount break-even hint |

## D. Adoption signal — honest read

Niche-of-niche, but with a real and engaged core. Evidence:
- Bee-dashboard top issue has 4 comments. swarm-desktop top issue has 5. Forum's #1 discussion is the welcome thread (4 comments). This is small.
- BUT: bee main repo issues attract 8–14 comments from core devs and recurring power-user names (ldeffenb, crtahlin, 0xCardiE, attila-lendvai, mfw78). The same ~20 operators show up across years — they're the ones who'd install a TUI.
- Two community tools already exist (Beest, Doctor Bee — both rampall/w3rkspacelabs) and are listed in awesome-swarm. Beest is process-management, not an operations cockpit; Doctor Bee is a one-shot health probe. Neither is a live TUI. **There is a clear gap.**
- Foundation has been investing in operator UX (warmup speedup, AutoTLS, postage snapshot, multi-underlay in 2.7.0). They want this audience to grow.

Verdict: 100s–low-1000s of serious operators today. A solid TUI gets adopted by the engaged core (~50–200 people on day one) and becomes the *de-facto* node-operator tool if it nails stamps + health + SWAP. It will never be "popular" — it will be loved by a small, vocal group, and that's enough.

## E. Surprises

- **Documentation is largely fine but the FAQ is tiny** — only ~25 questions total. The real institutional knowledge lives in GitHub issue threads (especially ldeffenb's comments on #4634, #4663). A TUI that surfaces "the right thing to look at" effectively encodes that tribal knowledge.
- **The community has explicitly proposed retiring depth+amount from user-facing docs** (#4992 mfw78 comment). Designing the TUI around volume+duration is *aligned with where the project itself is going* — not contrarian.
- **#5400 "Pushsync Silent Chunk Loss" (2026-03, 12 comments, OPEN, fix in PR #5390)** — chunks can vanish after "successful" upload. This is a *protocol-level* operator anxiety the TUI cannot fully solve, but it can mitigate by showing per-tag synced/seen counts and surfacing stewardship reupload status.
- **Logging is broken** — #4636 (loggerV2 messages don't appear at verbosity 5), #5360 ("control which logs to print"), #5342 ("log caller file:line"). Operators can't trust their own logs. A TUI that shows curated event streams beats `journalctl -u bee` for diagnosis.
- **Forum is dead, Discord is alive.** discuss.ethswarm.org top-of-all-time has single-digit replies; docs explicitly redirect to the `#node-operators` Discord channel. Real-time chat is where operators live — your TUI's "share my diagnostic bundle" feature should produce a Discord-paste-ready snippet.
- **2.7.0 just shipped pinned-chunk eviction protection and ENS-resolution-doesn't-crash** (blog 2026 release post). Operators were losing pinned data and being crashed by ENS until weeks ago. This audience has *low trust*; the TUI should make state changes (cashout, dilute, topup) confirmation-gated and dry-run-able.

## Sources

GitHub issues:
- bee/issues: #4992, #5400, #5282, #4663, #4607, #4102, #4634, #4746, #4292, #4849, #4194, #5216, #5428, #5396, #4941, #4636, #4266, #5142, #5096, #5203, #4864, #4991, #5450, #5403, #5380, #5355, #5388, #5386, #4419, #4131, #5123, #5129, #5219, #3122, #3092, #1297, #1787, #1816, #1945
- swarm-cli/issues: #509, #524, #499, #418, #479, #596, #684, #540, #705
- bee-dashboard/issues: #693, #636, #632, #563, #535
- swarm-desktop/issues: #481, #438, #514
- bee-js/issues: #525, #650, #762

Docs:
- docs.ethswarm.org/docs/bee/bee-faq, /staking, /connectivity, /set-target-neighborhood

Blog:
- blog.ethswarm.org/foundation/2026/bee-2-7-0-release
- blog.ethswarm.org/foundation/2025/monthly-development-update-may-2025

Community tools:
- github.com/rampall/beest
- github.com/rampall/doctor-bee
