# bee-rs audit for bee-tui (v1.2.0)

Audit performed on `/home/calin/work/swarm/bee-apis/bee-rs/` source.

## 1. Endpoint coverage

**Status / topology / node**
- `/health` ✅ `debug/node.rs:146` → `Health`
- `/readiness` ✅ `debug/node.rs:221` → `bool`
- `/node` ✅ `debug/node.rs:183` → `NodeInfo`
- `/status` ✅ `debug/node.rs:189` → `Status`
- `/status/peers` ✅ `debug/node.rs:197` → `Vec<PeerStatus>`
- `/status/neighborhoods` ✅ `debug/node.rs:209` → `Vec<Neighborhood>`
- `/addresses` ✅ `debug/peers.rs:130`
- `/topology` ✅ `debug/peers.rs:136`
- `/peers` ✅ `debug/peers.rs:71`
- `DELETE /peers/{addr}` ✅ `debug/peers.rs:93`
- `/blocklist` ✅ `debug/peers.rs:82`
- `/pingpong/{addr}` ✅ `debug/peers.rs:102`
- `/chainstate` ✅ `debug/node.rs:177`
- `/reservestate` ✅ `debug/peers.rs:142`
- `/redistributionstate` ✅ `debug/accounting.rs:234`

**Stamps**
- `GET /stamps`, `/stamps/{id}`, `/stamps/{id}/buckets`, `/batches`, `POST /stamps/{amount}/{depth}`, `/stamps/topup`, `/stamps/dilute` all ✅ `postage/endpoints.rs:32–145`

**Accounting / chequebook / settlements / stake**
- `/accounting`, `/balances`, `/balances/{addr}`, `/consumed`, `/consumed/{addr}` ✅ `debug/accounting.rs:130–168`
- `/chequebook/balance|cheque|cheque/{peer}|cashout/{peer}|deposit|withdraw` ✅ `debug/chequebook.rs:211–278`
- `/settlements`, `/settlements/{addr}` ✅ `debug/chequebook.rs:280–292`
- `/timesettlements` ❌ **MISSING** (no method, no struct)
- `/stake`, `/stake/withdrawable`, deposit/withdraw/migrate ✅ `debug/accounting.rs:182–229`
- `/rchash/{depth}/{a1}/{a2}` ❌ **MISSING**

**Tags / pins / stewardship / loggers / transactions**
- `/tags`, `/tags/{uid}` (CRUD) ✅ `api/endpoints.rs:70–131` → typed `Tag`
- `/pins`, `/pins/{ref}` ✅ `api/endpoints.rs:28–66`
- `/stewardship/{ref}` ✅ `api/endpoints.rs:134–155`
- `/loggers`, `/loggers/{exp}` (list/by-expr/PUT) ✅ `debug/loggers.rs:42–66`
- `/transactions`, `/transactions/{hash}` (GET/POST/DELETE) ✅ `debug/transactions.rs:68–123`

## 2. WebSocket support

**Already present.** `tokio-tungstenite 0.24` (rustls-tls-webpki-roots) in `Cargo.toml:21`. `pss/mod.rs:14,108` opens `connect_async`, spawns a reader task, exposes `Subscription { recv, cancel, Drop }` over `tokio::sync::mpsc::Receiver<Bytes>` with auto http→ws / https→wss promotion. `gsoc/mod.rs:73` reuses the same `Subscription`. **`/chunks/stream` upload WS is NOT implemented** — only `POST /chunks` (`file/chunk.rs:59`). The lift is small: ~150 lines, the existing `Subscription::open` pattern can be inverted to a `mpsc::Sender<Bytes>` writer.

## 3. Streaming downloads

`reqwest` is built with `stream` + `multipart` features (`Cargo.toml:19`). `download_data` (`file/data.rs:84`) buffers to `Bytes` but **`download_data_response` (`file/data.rs:96`) returns the raw `reqwest::Response`** — caller drives `.bytes_stream()` for progress. `bzz` mirrors it (`file/bzz.rs:101 download_file_response`). `probe_data` (`file/data.rs:109`) gets size via HEAD. Lib doc explicitly recommends the streaming variant for >hundreds of MB (lib.rs:138-149). **TUI is fully covered.**

## 4. Type ergonomics

Strongly typed throughout. Samples:
- `PostageBatch { batch_id: BatchId, amount: Option<BigInt>, depth: u8, immutable: bool, batch_ttl, utilization, usable, label, … }` (`postage/types.rs:17`) — wire bugs (`amount` vs `value`, `immutableFlag`) baked in.
- `Status { overlay, proximity, bee_mode, reserve_size, pullsync_rate: f64, connected_peers, … }` (`debug/node.rs:88`).
- `LastCheque`, `PeerCheques`, `LastCashoutAction`, `Settlement`, `Wallet` (`debug/chequebook.rs:43-160`), all `Deserialize + Clone + Debug + PartialEq`.
- `Tag { uid, total, split, seen, stored, sent, synced, address, started_at }` (`api/endpoints.rs:269`).
- `BigInt` for chain bigints. Custom deserializer for bigint-as-string fields in `chain_state`.

No raw-JSON escape hatch. TUI tables can render directly.

## 5. Multi-node ergonomics

`Client` is `#[derive(Clone, Debug)]`, `Arc<Inner>` under the hood (`client.rs:88-91`), `Send + Sync` (lib.rs:120). Construction: `Client::new(&str)` or `Client::with_http_client(&str, reqwest::Client)`. Holding N clients is free; sub-services (`client.file()`, `.debug()`) are zero-cost handles. **TUI-ready.**

## 6. Auth / TLS / tokens

No bearer / no admin-token API. The escape hatch is `Client::with_http_client` (`client.rs:115`) — caller builds `reqwest::Client` with `default_headers(HeaderMap)` (lib.rs:91-104 documents the pattern verbatim). No per-request header override; no `set_token()` method; no admin-restricted endpoint flag. Adequate for TUI but means token rotation requires a fresh `Client`.

## 7. Errors

`thiserror`-based, single enum (`swarm/errors.rs:27`):
`Argument { message } | Response { method, url, status, status_text, body } | LengthMismatch | Hex(#[from]) | Crypto(String) | Json(#[from]) | Transport(#[from] reqwest::Error) | Other`

Helpers: `Error::is_response()`, `Error::status() -> Option<u16>` (errors.rs:96-106). TUI can match `Transport(_)` for "node down", `Response { status: 401|403, .. }` for "auth failed", `Response { status: 404, .. }` for "not found". **Pattern-matchable, perfect for banners.**

## 8. Open issues / TODOs

Source has zero `TODO|FIXME|XXX`. Parity-plan (`bee-rs-parity-plan.md`) shows P0–P5 closed; P2 status snapshot (line 414) lists **only** Bzz/Dai/Duration/ResourceLocator (all done). 223 tests pass. Remaining P1 callout is `postage::Stamper` items finished in P5; only `stream_directory`/`stream_collection_entries` flagged as still open in line 150 — but `file/stream.rs:92,106` already ships them.

---

## Punch list — what bee-rs needs before bee-tui ships

1. **`/chunks/stream` WebSocket upload** (medium) — required for live upload-tracker. Reuse `Subscription::open` pattern inverted (`mpsc::Sender<Bytes>` + sink). ~150 LOC + 1 wiremock test.
2. **`/rchash/{depth}/{a1}/{a2}` endpoint** (small) — ~30 LOC in `debug/accounting.rs`. Needed for redistribution-debug screens.
3. **`/timesettlements` endpoint** (small) — ~25 LOC; same shape as `Settlements`. Needed for time-settlement screen.
4. **Auth ergonomics** (small) — `Client::with_token(url, token)` constructor, or a `Client::set_default_header(name, val)` mutator that rebuilds the inner `reqwest::Client`. Avoids the TUI re-implementing header-map plumbing.
5. **`tracing` spans** (small, optional) — lib.rs:177 admits `tracing` is a dep but unused. TUI debug pane benefits from per-request spans. Wrap `Inner::send` with `#[tracing::instrument]`.
6. **No-op nice-to-have**: `Client::ping() -> Duration` helper (HEAD `/health` + timed) for the connection-status bar. Trivial to add downstream.

Items 1–3 are real gaps. 4–6 are polish. Everything else the TUI needs is in v1.2.0 today.
