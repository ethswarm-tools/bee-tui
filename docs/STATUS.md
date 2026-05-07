# bee-tui — session status

**Last updated:** 2026-05-07 mid-session
**Phase:** v0.1 in flight — S1 Health + S10 Command-log shipped; S2 Stamps + multi-screen routing remain.

## Today (2026-05-07)

### Repo + reservation

- ✅ Scaffolded from upstream `ratatui/templates component`
- ✅ Customized: Cargo.toml metadata, `ethswarm-tools` ProjectDirs, dual MIT/Apache license, README
- ✅ Planning + research moved to `docs/`
- ✅ `git init`, GitHub repo created at https://github.com/ethswarm-tools/bee-tui (public)
- ✅ Initial commit + push (martinconic only, no Co-Authored-By)
- ✅ CI iterated to green (Liquid `{% raw %}` strip + clippy `let_and_return` fixes)
- ✅ **`bee-tui v0.0.1` published to crates.io** as crate-name reservation: https://crates.io/crates/bee-tui/0.0.1

### v0.1 implementation chunks shipped

- ✅ `bee-rs ≥ 1.3` dependency wired into Cargo.toml
- ✅ Config: `[[nodes]]` schema with `default localhost:1633`, `@env:VAR_NAME` token indirection
- ✅ `src/api/ApiClient` — wraps `bee::Client` + `NodeConfig`; honors token on `with_token`
- ✅ `src/watch/BeeWatch` — k9s-style informer hub; spawns `HealthSnapshot` poller @ 2 s, hierarchical `CancellationToken`s
- ✅ `src/components/health::Health` — S1 with 9 operator-pain-validated gates including the 30-min wakeup-tick tooltip
- ✅ `src/log_capture` — ring buffer + `tracing-subscriber` Layer that captures `bee::http` events
- ✅ `src/components/command_log::CommandLog` — S10 lazygit-style request tail
- ✅ App layout split: Health top, Command-log strip 8 lines bottom
- ✅ Lib + bin restructure so integration tests can reach internal modules
- ✅ 27 tests (24 unit + 3 insta snapshots) — all pass
- ✅ All commits clean fmt + `clippy --all-targets -- -D warnings`

### Outstanding

- ⚠️ **Two crates.io tokens leaked into chat history** (`cioE…dmt8zyM`, `ciot…NKUQwH`) — rotate at https://crates.io/settings/tokens
- ⚠️ CD workflow (`cd.yml`) failed on the `v0.0.1` tag push — release-binary cross-compile pipeline needs investigation. Non-blocking: `cargo install bee-tui` works.
- ⚠️ S1 still has a `Bin saturation` placeholder gate; needs `/topology` poller (slated for v0.2)

## Where we are vs the plan

| PLAN.md screen | Status |
|---|---|
| S1 Health gates | ✅ shipped (9 of 10 gates; bin saturation deferred) |
| S2 Stamps | 🔜 next chunk (volume+duration UX + bucket histogram) |
| S3 SWAP / cheques | ⏳ v0.2 |
| S4 Lottery | ⏳ v0.2 |
| S5 Warmup | ⏳ v0.3 |
| S6 Peers + bin saturation | ⏳ v0.3 |
| S7 Network / NAT | ⏳ v0.3 |
| S8 RPC health | ⏳ v0.3 |
| S9 Tags + uploads (WS) | ⏳ v0.5 |
| S10 Command log | ✅ shipped |

## Next move

Per PLAN.md § 12 the v0.1 scope is **S1 + S2 + S10**. Two of three landed today. Next chunk:

1. **Add `/stamps` polling** to `BeeWatch` (10 s cadence per the refresh-policy table)
2. **Build `src/components/stamps::Stamps`** — volume+duration UX leading, bucket histogram on focus, `usable` countdown
3. **Replace the layout split with router-style routing** so `:command` can switch between Health and Stamps screens
4. **Insta snapshots** for the Stamps screen (passing / pending-usable / overflow-bucket cases)
5. **Bind `Tab` or `:` for screen switching**

After S2 lands the v0.1 milestone is complete and we can cut a real `0.1.0` release.

## Reading order

1. `STATUS.md` (this file)
2. `PLAN.md` § 8.S2 — Stamps screen spec
3. `research/06-bee-internals.md` § 1 — stamp internals (MaxBucketCount truth-telling, bucket overflow, usable confirmation gate)
4. `research/05-operator-pain-points.md` § A.1 + A.2 — depth/amount confusion + bucket-collision pain
