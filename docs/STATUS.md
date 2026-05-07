# bee-tui — session status

**Last updated:** 2026-05-06 EOD
**Phase:** pre-implementation. Plan v2 canonical. No code yet.

## Today (2026-05-06)

- ✅ TUI existence survey — confirmed no Bee TUI exists anywhere
- ✅ Stack analysis — Rust + Ratatui + bee-rs settled
- ✅ Plan v1 drafted; superseded by v2 after deep research
- ✅ Four parallel research streams completed:
  - operator pain points (top 10 with GitHub issue evidence)
  - Bee subsystem internals (stamps, redistribution, accounting, sync, reserve)
  - decentralized-storage TUI prior art (lntop graveyard, k9s playbook gap)
  - 2026 Ratatui state-of-the-art (versions, patterns, hidden gems)
- ✅ Plan v2 written to disk + 8 research files persisted under `research/`
- ✅ **bee-rs v1.3.0 SHIPPED** — closed all 6 prereq gaps:
  - `Client::ping`
  - `Client::with_token`
  - `tracing::debug!` on `Inner::send` (target `bee::http`)
  - `DebugApi::time_settlements`
  - `DebugApi::r_chash` + 5 new types
  - `FileApi::chunks_stream` + `ChunkStream` (WS upload)
- ✅ 247 tests pass (added 9 in `tests/v1_3.rs`)
- ✅ Live-validated against Bee 2.7.2-rc1 on Sepolia (6/6 incl. WS upload with batch `4850…2c4a`)
- ✅ Released to crates.io: https://crates.io/crates/bee-rs/1.3.0
- ✅ GitHub release: https://github.com/ethswarm-tools/bee-rs/releases/tag/v1.3.0
- ⚠️ **Action required:** rotate the crates.io token pasted into chat (`cioE…`)

## Tomorrow's first move

Per [PLAN.md § 12](PLAN.md) — start **bee-tui v0.1**:

1. **Decide open items first** (PLAN.md § 14):
   - Repo location? (suggested: `github.com/ethswarm-tools/bee-tui`)
   - License? (suggested: match bee-rs — MIT OR Apache-2.0)
   - Reserve `bee-tui` on crates.io via dummy publish?
2. **Scaffold the project:**
   ```bash
   cargo generate --git https://github.com/ratatui/templates component bee-tui
   ```
3. **First screen to build: S1 Health gates** (PLAN.md § 8.S1)
   - Data sources: `/health`, `/readiness`, `/wallet`, `/status`,
     `/redistributionstate`, `/chainstate`, `/topology`
   - Layout: vertical list of gates, green/red with WHY tooltips
   - Encoded knowledge: explain the 30-min reserve-worker tick when
     `storageRadius < committedDepth`
4. **Concurrent with S1:** wire the watch/informer layer
   (`src/watch/`) + the Action enum + `tokio::select!` event loop
   from the component template
5. **Concurrent with S1:** S10 Command log pane subscribes to the
   `bee::http` tracing target — operators watch live API traffic

## v0.1 acceptance criteria

- Single-node only (multi-node is v0.4)
- Three screens: S1 Health, S2 Stamps, S10 Command log
- Config via `~/.config/bee-tui/config.toml` (config 0.15)
- Tracing to `$XDG_DATA_HOME/bee-tui/log/bee-tui.log`
- CI matrix: Linux/macOS/Windows × stable + 1.85 MSRV
- insta snapshot tests for every component at 80×24, 132×40, 200×60
- wiremock integration tests for every Bee endpoint touched

## Reading order tomorrow

1. `STATUS.md` (this file) — pin
2. `PLAN.md` § 4 (scope) + § 6 (architecture) + § 8.S1, S2, S10 (screen specs)
3. `research/05-operator-pain-points.md` § C — the "why is my node X" anxieties S1 must answer
4. `research/06-bee-internals.md` § 1 (stamps) + § 2 (redistribution) — for S1's encoded-knowledge tooltips
