# Prometheus metrics

bee-tui can expose a Prometheus `/metrics` endpoint with the gauges
the cockpit screens already compute. The point isn't to duplicate
Bee's own `/metrics` — Bee exposes plenty of infrastructure
counters — it's to make bee-tui's *unique* synthesised gauges
machine-readable so a Grafana board can graph them alongside Bee's:

- **Per-batch worst-bucket fill** — predicts upload-failure
  before Bee's API admits anything is wrong.
- **Predictive stamp economics** — depth, capacity bytes, TTL
  seconds per batch (same math as
  [`:*-preview`](../commands/stamp-previews.md)).
- **Pending-tx age** — operator-relevant signal that Bee surfaces
  only as a creation timestamp.
- **Depth-vs-radius gap** — `committed_depth - storage_radius`;
  positive means the node hosts chunks beyond its storage radius
  (chunk-loss risk during shrinkage).
- **bee-tui's own request stats** — p50/p99/error-rate over the
  recent client-side log-capture window. Distinguishes "Bee is
  slow" from "the network between bee-tui and Bee is slow".

## Enable it

In `config.toml`:

```toml
[metrics]
enabled = true
addr    = "127.0.0.1:9101"   # default; only opt into 0.0.0.0 if you mean it
```

Off by default — exposing an HTTP listener should be a deliberate
operator opt-in, even on localhost.

## Scrape config

Standard Prometheus drop-in:

```yaml
scrape_configs:
  - job_name: bee-tui
    static_configs:
      - targets: ['127.0.0.1:9101']
    scrape_interval: 30s
```

bee-tui re-renders the metrics on every scrape, reading the latest
snapshots from the same watch channels the screens use — so the
values match what the operator sees in the cockpit at the moment of
the scrape.

## Metric reference

All metrics are namespaced `bee_tui_`. Gauges unless noted.

### Liveness + identity

| Metric | Labels | Description |
|---|---|---|
| `bee_tui_up` | — | Always `1` if the scrape responds |
| `bee_tui_info` | `version`, `overlay`, `bee_mode` | Always `1`; metadata via labels |
| `bee_tui_resource_loaded` | `resource` | `1` if that resource's last poll succeeded (`health` / `stamps` / `swap` / `lottery` / `topology` / `network` / `transactions`) |

### Status (`/status`)

| Metric | Description |
|---|---|
| `bee_tui_status_connected_peers` | `Status.connectedPeers` |
| `bee_tui_status_neighborhood_size` | `Status.neighborhoodSize` |
| `bee_tui_status_reserve_size_chunks` | `Status.reserveSize` |
| `bee_tui_status_reserve_size_within_radius_chunks` | `Status.reserveSizeWithinRadius` |
| `bee_tui_status_storage_radius` | `Status.storageRadius` |
| `bee_tui_status_committed_depth` | `Status.committedDepth` |
| `bee_tui_status_depth_radius_gap` | `committedDepth - storageRadius` (synthesised) |
| `bee_tui_status_is_reachable` | 0 / 1 |
| `bee_tui_status_is_warming_up` | 0 / 1 |
| `bee_tui_status_last_synced_block` | `Status.lastSyncedBlock` |
| `bee_tui_status_proximity` | `Status.proximity` |
| `bee_tui_status_batch_commitment` | `Status.batchCommitment` |
| `bee_tui_status_pullsync_rate_per_second` | `Status.pullsyncRate` (chunks/sec) |

### Chain (`/chain-state`)

| Metric | Description |
|---|---|
| `bee_tui_chain_block` | Local block height |
| `bee_tui_chain_tip` | Highest block observed |
| `bee_tui_chain_lag_blocks` | `tip - block` (synthesised) |
| `bee_tui_chain_current_price_plur` | Per-chunk PLUR/block price |

### Postage (`/stamps`)

`bee_tui_stamps_count` is unlabelled; the per-batch metrics carry
`{batch_id, label}` so a Grafana panel can graph them per batch.

| Metric | Description |
|---|---|
| `bee_tui_stamps_count` | Total batches |
| `bee_tui_stamp_worst_bucket_ratio` | Worst-bucket fill `0..1` (S2's worst-bucket %) |
| `bee_tui_stamp_ttl_seconds` | Predicted TTL |
| `bee_tui_stamp_depth` | Batch depth |
| `bee_tui_stamp_capacity_bytes` | `2^depth × 4096` |
| `bee_tui_stamp_immutable` | 0 / 1 |
| `bee_tui_stamp_usable` | 0 / 1 (chain-confirmed) |

### Pending transactions

| Metric | Description |
|---|---|
| `bee_tui_pending_tx_count` | Number of pending Bee transactions |
| `bee_tui_pending_tx_oldest_age_seconds` | Age of the oldest pending tx |

### bee-tui's own client-side requests

Same window as the S8 RPC/API screen.

| Metric | Description |
|---|---|
| `bee_tui_self_request_sample_size` | Entries contributing to the percentile math |
| `bee_tui_self_request_latency_p50_seconds` | Median latency (omitted when no samples) |
| `bee_tui_self_request_latency_p99_seconds` | 99th-percentile latency |
| `bee_tui_self_request_error_ratio` | Fraction `0..1` with status ≥ 400 |

### SWAP / Lottery / Topology / Network

| Metric | Description |
|---|---|
| `bee_tui_swap_chequebook_total_plur` | Total chequebook balance (PLUR) |
| `bee_tui_swap_chequebook_available_plur` | Uncashed balance (PLUR) |
| `bee_tui_lottery_staked_plur` | Currently staked BZZ in PLUR |
| `bee_tui_topology_population` | Peers known across all bins |
| `bee_tui_topology_connected` | Currently connected peers |
| `bee_tui_topology_depth` | Kademlia depth |
| `bee_tui_topology_radius` | Nearest-neighbour low watermark |
| `bee_tui_network_underlay_count` | Underlay multiaddr count from `/addresses` |

## Wire format

`Content-Type: text/plain; version=0.0.4; charset=utf-8` — the
standard Prometheus text exposition format. Each metric family
emits a `# HELP` and `# TYPE` line followed by one sample line.
Label values are escaped per the Prometheus spec (`\\`, `\"`,
`\n`).

## Security notes

- Default bind is `127.0.0.1`. If you set `addr = "0.0.0.0:..."`,
  you've opted into reachability from any interface — put a
  firewall in front.
- The endpoint exposes batch IDs and the node's overlay address.
  These are public on-chain values but worth knowing if you
  proxy the endpoint through a reverse proxy you don't control.
- No authentication. Prometheus's standard answer is to bind
  scrapers behind a private network or use mTLS at the proxy
  layer.
