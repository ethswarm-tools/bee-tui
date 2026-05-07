# Bee Internal State for a TUI ("bee-tui")

Deep dive into Bee subsystem internals — what state Bee tracks internally and what's actually exposed to operators via the API.

## 1. Postage stamps

**Concepts**
- `BatchDepth` (uint8) defines batch capacity = `2^depth` chunks (`pkg/postage/stampissuer.go:127,250-252`).
- `BucketDepth` partitions the address space into `2^bucketDepth` neighborhoods; each holds `2^(depth-bucketDepth)` chunks (`stampissuer.go:128,247-252`). On mainnet `BucketDepth=16` (`pkg/postage/postagecontract/contract.go:25`).
- `Amount` is the per-chunk normalised price already paid; the chain has a global `chainstate.TotalAmount` that grows with `CurrentPrice` per block. **TTL** isn't stored — it's `(Value - TotalAmount) / CurrentPrice * blockTime`. Eviction happens when `b.Value <= cs.TotalAmount` (`pkg/postage/batchstore/store.go:308`).
- `Utilization` returned by the API is just `MaxBucketCount` (the count of the **fullest** bucket), not an average (`stampissuer.go:218-223`).
- `usable` is purely a chain-confirmation gate: false until `cs.Block - st.BlockNumber >= 10` blocks (`pkg/postage/service.go:218-228`, `blockThreshold = 10`).

**Bucket collisions**
- For each chunk, bucket is the top `bucketDepth` bits of the address (`stampissuer.go:371-373`). When a bucket reaches `BucketUpperBound = 1 << (depth-bucketDepth)`:
  - **Immutable** batches return `ErrBucketFull` (`stampissuer.go:186-189`) — the upload fails even though `utilization` < theoretical max and `batchTTL` is large.
  - **Mutable** batches silently overwrite from index 0, invalidating the previously stamped chunk (`stampissuer.go:191-193`). The stamp is "still usable" per `IssuerUsable` but earlier chunks lose their stamp.
- The fullest bucket fills disproportionately fast because address distribution is non-uniform; `MaxBucketCount` can hit 100% while average is ~50%.

**Recovery** — On a dirty restart, bucket counts are re-derived from the stamp store (`pkg/postage/service.go:106-127`). Until that finishes there is a window of incorrect collision counts.

**API exposure** — `/stamps`, `/stamps/{id}`, `/stamps/{id}/buckets` (`openapi/Swarm.yaml:1877-1945`). `PostageBatch.utilization` is `MaxBucketCount`, `PostageStampBuckets` returns full per-bucket counts (`SwarmCommon.yaml:564-585`). NOT exposed: `dirty` flag, `MaxBucketCount` *vs* second-fullest gap, time-to-bucket-exhaustion, the in-memory `stampIssuerData.Buckets` only flushes every 60 s (`service.go:33,131-153`) so `/buckets` can lag by up to a minute after upload.

**TUI win**
- Bucket histogram with the imminent-overflow bucket highlighted; ETA-to-full = `(BucketUpperBound - max) / current_chunks_per_sec`.
- "Effective remaining capacity" = `2^bucketDepth × (BucketUpperBound − MaxBucketCount)` instead of the raw `2^depth - utilization×N` lie.
- Confirmation countdown for `usable: false` (`block_now - blockNumber` toward 10).

**Gotcha** — `utilization=80%` does *not* mean 20% headroom; an immutable batch can be unusable when one bucket is at 100% while utilization is well below max.

## 2. Redistribution game

**Round / phases** (`pkg/storageincentives/agent.go:36-44, 130-235`): every `blocksPerRound=152` blocks split into 4 quarters → 38 blocks each. Phases by block-mod:
- `commit` `[0,38)` — submits obfuscated hash from the **previous round's** sample (`agent.go:263-289`).
- `reveal` `[38,76)` — reveals key+sample. Skipped if no commit was sent that round (`agent.go:291-319`).
- `claim` `[76,152)` — `IsWinner` check, on-chain claim (`agent.go:321-392`). `sample` is published at the start of claim (`agent.go:156-178`) and runs through the next commit phase.

**Pre-conditions to play** (`agent.go:394-447`): `!IsFrozen`, `IsPlaying(committedDepth)` true (anchor-prefix selects the neighborhood), `IsFullySynced`, `IsHealthy`, plus EOA balance ≥ `minTxCountToCover * avgTxGas * gasPrice` ≈ ~15 tx of ~250k gas (`agent.go:39-43, 613-628`).

**Depth confusion** — `StorageRadius` = bin radius from kademlia/reserve (`pkg/storer/reserve.go:420-425`); `CommittedDepth = StorageRadius + capacityDoubling` (`reserve.go:427-433`). The contract's `IsPlaying` uses `committedDepth`, but the reserve sample is taken at `committedDepth` proximity — operators see two depths and don't know which one matters where.

**isFrozen** (`pkg/storageincentives/staking/contract.go` + `agent.go:227-233`) is set per round by `IsOverlayFrozen(block+1)`. Caused by submitting an invalid commit/reveal or running with desynced reserve. Freeze duration is set on-chain (typically `blocksPerRound × penalty`).

**Status fields** (`pkg/storageincentives/redistributionstate.go:43-58`): `Phase`, `Round`, `LastWonRound`, `LastPlayedRound`, `LastFrozenRound`, `LastSelectedRound`, `Reward`, `Fees`, `SampleDuration`, `IsFrozen`, `IsFullySynced`, `IsHealthy`. `RoundData[round]{CommitKey, SampleData, HasRevealed}` keeps the last 10 rounds (`redistributionstate.go:327-350`).

**API exposure** — `/redistributionstate` returns Status (`openapi/Swarm.yaml:2145`). NOT exposed clearly: which precondition caused a "skip" (insufficient funds vs not-selected vs unhealthy), per-phase metrics (`metrics.go` has `CommitPhase`/`RevealPhase`/`ClaimPhase`/`InsufficientFundsToPlay` only via Prometheus), `SampleDuration` from current round.

**TUI win**
- Timeline strip: `commit | reveal | claim | sample` with current block tick and per-phase outcome ("committed", "skipped: not playing", "skipped: frozen until block X").
- "Why didn't I win" panel: shows for last N rounds → `selected? committed? revealed? winner?` and the reason for any No.
- Earnings ledger: `Reward − Fees` cumulative since process start, mapped by `LastWonRound`.

**Gotcha** — `LastPlayedRound` only updates on commit (`agent.go:286`); a node that's selected but skips the round (frozen, broke, unhealthy) leaves `LastPlayedRound` stale. Operators read "last played round 14512" and assume the node is fine.

## 3. Accounting / SWAP / cheques

**Per-peer state** (`pkg/accounting/accounting.go:131-151`): `paymentThreshold` (we owe → settle), `earlyPayment` (start settling earlier to avoid blocking), `paymentThresholdForPeer` (peer should pay us at this), `disconnectLimit` = `(100+tolerance)% × paymentThreshold` (`accounting.go:226`), `ghostBalance` (debt we *could* have charged but didn't due to refresh slack), `shadowReservedBalance` (in-flight credits the peer might already see).

**Transitions**
- Credit attempt: if `expectedDebt − shadowReserved ≥ earlyPayment` and we're already in debt → `settle()` (`accounting.go:286-312`).
- If still over `paymentThreshold + refreshRate × elapsed` after refresh → `ErrOverdraft`, request blocked (`accounting.go:314-323`).
- `disconnectLimit`: peer crossing it disconnects them with `ErrDisconnectThresholdExceeded` (`accounting.go:196`).

**Settlement choice** — Two settlement paths: `pseudosettle` (time-based, free, refresh) and `swap` (cheque, on-chain). Refresh runs first; only when balance still exceeds `minimumPayment = refreshRate/5` (`accounting.go:43,234`) is a cheque issued via `swap.Pay → chequebook.Issue` (`pkg/settlement/swap/chequebook/chequebook.go:62`).

**Cheque cashing** — Cheques issued from us are stored cumulative; received cheques accumulate. `CashCheque` is **manual** unless the cashout service auto-cashes when the value − last cashed exceeds a threshold (`pkg/settlement/swap/chequebook/cashout.go`). A cheque whose chequebook contract has insufficient balance returns `ChequeBounced` (`chequebook.go:43-44`).

**API exposure** — `/balances`, `/balances/{addr}`, `/timesettlements` (pseudosettle = refresh totals), `/settlements` (cheque-based cumulative), `/accounting` (full per-peer fields incl. `ghostBalance`), `/chequebook/cheque/*`, `/chequebook/cashout/{peer}` (`Swarm.yaml:1159-1290, 1520-1580, 1632-1771, 2128`). NOT exposed: `paymentOngoing`/`refreshOngoing` flags (in-flight settlement state), `lastSettlementFailureTimestamp`, `disconnectLimit` per peer, `connected` bool per accountingPeer.

**TUI win**
- Per-peer "debt thermometer" with three lines: balance / earlyPayment / paymentThreshold / disconnectLimit, color-coded.
- "Cashout candidates" panel: received cheques ranked by `(cumulativePayout - lastCashedPayout)` with chequebook on-chain balance — instantly answers "why aren't my cheques being cashed" (chequebook out of funds vs threshold not reached vs cashout never triggered).
- Refresh-vs-swap split: how much of `totalReceived` came from time-based vs cheques per peer.

**Gotcha** — `surplusBalance` is debt the peer owes *us* from incoming overpayment; it is subtracted before deciding to settle (`accounting.go:260-267`). A high positive balance with high surplus means we're not actually owed — a TUI showing `balance` alone misleads.

## 4. Sync — push / pull / retrieve

**push vs pull** — Push (`pkg/pushsync/pushsync.go`) is the originator-driven forward-to-closest with a `Receipt`; TTL 30 s, preemptive forward at 5 s (`pushsync.go:48-49`). Pull (`pkg/pullsync/pullsync.go:46-51`) is the neighbor-driven historical sync that walks bin cursors at 250 chunks/sec/peer, page-size 250.

**`pullsyncRate`** is the rolling average of historical pull-sync chunks per second across all peers — exposed via `/status` (`SwarmCommon.yaml:897`). Comes from `Syncer.SyncRate()` used in `reserveWorker` to decide if it's safe to lower radius (`pkg/storer/reserve.go:176`).

**Proximity & connectedPeers** — Bin `i = Proximity(self, peer)` ranges 0..MaxPO. Saturation defaults: `SaturationPeers=8`, `OverSaturationPeers=18` (`pkg/topology/kademlia/kademlia.go:54-55`). Below saturation a bin is "shallow" → kademlia keeps dialing.

**`storageRadius` vs `committedDepth`** — `storageRadius` = the bin from which the node accepts/keeps chunks (`reserve.go:420`). It can be **lowered** by the reserve worker if reserve fill is below 50% capacity, sync is idle, AND above `MinimumStorageRadius` (`reserve.go:45,176-184`). It is **raised** when capacity is exceeded (`reserve.go:366-401`). `committedDepth = storageRadius + capacityDoubling` is what the redistribution contract checks.

**Warmup lifecycle** — `stabilization.Subscriber` gates the reserve worker and sync start (`pkg/storer/reserve.go:61-86`). During warmup the node returns `ErrWarmup` for push (`pushsync.go:62`) and `IsWarmingUp=true` in `/status`.

**Hidden sync queue state** — `pullsync.syncInProgress` atomic counter (`pullsync.go:74`), `pushsync.errSkip` skiplist of peers that errored within the last 5 minutes (`pushsync.go:50,99`), `overDraftRefreshLimiter` rate-limits payment-blocked retries to 600 ms (`pushsync.go:51,103`).

**API exposure** — `/topology` exposes per-bin population, `connectedPeers`, `disconnectedPeers` with metrics, depth, reachability. `/status` has `pullsyncRate`, `connectedPeers`, `neighborhoodSize`, `committedDepth`, `storageRadius`, `isReachable`, `isWarmingUp`. NOT exposed: per-bin pull cursors / sync progress, the active push-sync errSkip set, current push concurrency saturation, per-peer pullsync `topmost` BinID.

**TUI win**
- Bin saturation bar (per-bin connected / saturation target / oversaturation), making it obvious which bins are starving and explain failed lookups.
- Push success rate + ErrShallowReceipt count over 1 min — the dominant "my upload is slow" cause.
- Sync cursor map: per neighbor, current synced BinID vs their reported MaxCursor, so an operator sees the actual gap closing in chunks.

**Gotcha** — `storageRadius` decreases happen **only on the 30-min wakeup tick** (`storer.go:411`, `reserveOptions.wakeupDuration`). Operators watching radius live see no change for half an hour even when reserve is empty.

## 5. Chunk reserve

**ReserveSize** = chunk count across all bins. **ReserveSizeWithinRadius** counts only chunks whose bin ≥ `storageRadius` (`pkg/storer/reserve.go:88-119`); this is the metric that should approach capacity in steady state. The atomic at `reserve.go:36` is updated only by `countWithinRadius` runs.

**Eviction** — `unreserve()` (`reserve.go:331-405`): when over capacity, iterate batches and evict chunks at `radius`, then `radius+1`, until `EvictionTarget` reached. Each eviction round bumps `storageRadius`. `MinEvictCount` default forces a minimum eviction per batch.

**Reserve threshold** = `capacity × 0.5` (`reserve.go:45`). Below that and sync idle → radius decreases (more catchment area). Above capacity → radius increases (smaller area).

**Batch eviction triggers** (`reserve.go:27-32`): `reserveOverCapacity` (live), `batchExpiry` (chain expiry), `reserveUnreserved` (post-eviction notify). Expired batches are recorded in `expiredBatchItem` then drained (`reserve.go:188-227`).

**API exposure** — `/reservestate`, `/status` fields. `/status/neighborhoods` exposes per-neighborhood `ReserveSizeWithinRadius` (`SwarmCommon.yaml:928-950`). NOT exposed: per-bin chunk counts, per-batch reserve fill, eviction log, time-since-last-eviction, the `expiredBatchItem` queue length.

**TUI win**
- Reserve fill gauge per bin (stacked bar) + capacity line — instantly shows skewed distribution before the radius re-balances.
- "Evictions in last 24h" with batch labels & chunk counts — answers "am I dropping chunks I shouldn't?" (eviction below radius = bad).
- ReserveMissingBatch counter (already in metrics, `reserve.go:115`) surfaces stamps that reference batches the node never saw.

**Gotcha** — A chunk pushed into the reserve at bin `< storageRadius` is **not counted** in `reserveSizeWithinRadius` and is the first to be evicted; uploads to peers far from your overlay show as healthy `reserveSize` growth but contribute zero to redistribution sample.

## 6. Health & reachability

`isReachable` comes from libp2p AutoNAT (`pkg/p2p/libp2p/`). `--full-node` flips `fullNode=true` → node accepts pushsync forwards, runs pullsync historical, becomes neighborhood-eligible. Light nodes get `lightPaymentThreshold = paymentThreshold/lightFactor` (`accounting.go:219-220`).

**API exposure** — `/health` (status/version/apiVersion), `/readiness`, `/topology.reachability`, `/status.isReachable`. NOT exposed: AutoNAT dial failure counts, NAT type (cone/symmetric), libp2p relay candidates.

**TUI win**
- NAT diagnostics: AutoNAT dial-back attempts vs successes; relay/holepunch state.
- Bee mode badge (`light|full|ultra-light|unknown` from `/status.beeMode`) prominent with implications listed (cannot win redistribution if not full, etc.).

**Gotcha** — `isReachable=true` means *some* peer dialed back successfully; under symmetric NAT it can flip true/false depending on which peer probes. Show stability over 10 min, not a snapshot.

---

## Key files referenced

- `pkg/postage/{stampissuer.go,service.go,stamper.go,batch.go,batchstore/store.go,postagecontract/contract.go}`
- `pkg/storageincentives/{agent.go,redistributionstate.go,staking/contract.go}`
- `pkg/accounting/accounting.go`
- `pkg/settlement/swap/{swap.go,chequebook/chequebook.go,chequebook/cashout.go}`
- `pkg/pushsync/pushsync.go`
- `pkg/pullsync/pullsync.go`
- `pkg/topology/kademlia/kademlia.go`
- `pkg/storer/{storer.go,reserve.go}`
- `openapi/{Swarm.yaml,SwarmCommon.yaml}`
