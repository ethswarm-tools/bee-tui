# bee-tui state-of-the-art audit (May 2026)

Audit of current Ratatui ecosystem — versions, patterns, hidden gems, and stale recommendations to retire.

## A. 2026 Library BoM (verified on crates.io)

| Crate | Version | Notes |
|---|---|---|
| `ratatui` | **0.30.0** (2025-12-26) | Major modular workspace rewrite; `ratatui::run()`; `no_std`; new `Flex::SpaceEvenly`; pluggable backends via feature flags `crossterm_0_28` / `crossterm_0_29`. No 1.0 yet — pre-1.0 breakage policy still applies. |
| `crossterm` | **0.29.0** (2025-04-05) | Pin to this; it's the version ratatui 0.30 targets natively. |
| `tokio` | **1.52.2** (2026-05-04) | Use `features = ["full"]` for TUI work. |
| `tokio-util` | **0.7.18** (2026-01-04) | Bring in for `CancellationToken` + `StreamExt`. |
| `color-eyre` | **0.6.5** (2025-05-30) | 2026 default for TUIs (panics restore terminal cleanly). |
| `eyre` | 0.6.12 | Pulled transitively; no need to depend directly. |
| `anyhow` | 1.0.102 | **Don't mix with eyre.** `color-eyre` is the TUI choice. |
| `tracing` | 0.1.44 | |
| `tracing-subscriber` | **0.3.23** (2026-03-13) | |
| `tracing-appender` | **0.2.5** (2026-04-17) | New non-blocking writer flush ergonomics. |
| `figment` | 0.10.19 (last update 2024-05) | Stagnating. |
| `config` | **0.15.22** (2026-03-17) | Now the de-facto config-loading default in 2026 — actively maintained, layered providers, async-friendly. |
| `serde` | 1.0.228 | |
| `tui-input` | **0.15.3** (2026-04-18) | Still the simple line-input pick. |
| `tui-textarea` | 0.7.0 (2024-10-22) | Slowing — usable but check before adopting. |
| `tui-tree-widget` | **0.24.0** (2026-01-09) | |
| `tui-popup` | **0.7.4** (2026-04-04) | |
| `throbber-widgets-tui` | **0.11.0** (2026-02-22) | |
| `tachyonfx` | **0.25.0** (2026-02-27) | **Now under `ratatui/tachyonfx`** — junkdog handed it to the org. The 2026 standard for animations. |
| `tui-realm` | (still around) | See section B — eclipsed for new starts. |
| `rat-salsa` | **4.0.3** (2026-03-08) | The other component framework — see D. |
| `insta` | 1.47.2 (2026-03-30) | Still THE snapshot tool. |
| `cargo-dist` | **0.31.0** (2026-02-23) | The "rename to `dist`" is documented but the **package is still `cargo-dist` on crates.io**. The `dist` crate name is squatted (v0.0.0, 2016). Advertise as `cargo-dist` for now. |
| `tokio-tungstenite` | **0.29.0** (2026-03-17) | Default WebSocket client. |
| `fastwebsockets` | 0.10.0 | Server-side throughput choice; for a client TUI use tokio-tungstenite. |
| `nucleo` / `nucleo-matcher` | 0.5.0 / 0.3.1 (2024) | No newer release — still the matcher of record (television uses it). |
| `ratzilla` | 0.3.0 (2026-01-23) | Optional: ship a web build of bee-tui via WASM with the same ratatui code. |

### Version-skew gotchas

- ratatui 0.30 + crossterm 0.29 work natively, but third-party widget crates may still pin crossterm 0.28. Use ratatui's `crossterm_0_28` feature flag if you hit a transitive widget that needs it.
- `nucleo` hasn't shipped since 2024 — fine, but don't expect bug fixes.
- `figment` last touched 2024-05; if you don't already use it, prefer `config 0.15`.

## B. Stale recommendations to retire

1. **`anyhow` for TUIs.** 2023–2024 advice. `color-eyre` is now the default — its panic hook restores the terminal automatically. (https://github.com/eyre-rs/color-eyre)
2. **`tui-rs`.** Dead since 2023; ratatui is the fork.
3. **"Use figment" as the default config story.** Maintenance has slowed; `config` 0.15 is the active option.
4. **`tui-realm` as your starting point.** Still maintained but the community has moved to either (a) the official `ratatui/templates` Component template or (b) `rat-salsa` for heavier event-queue needs. Don't pick tui-realm for a greenfield 2026 app unless you have prior code.
5. **Hand-rolling an event loop with raw `crossterm::event::read()` in a thread.** The 2026 idiom is `EventStream` from `crossterm` driven through `tokio::select!`, with separate tick/render intervals — see Ratatui's "Async Event Stream" tutorial. (https://ratatui.rs/tutorials/counter-async-app/async-event-stream/)
6. **Nerd Fonts as default glyphs.** Still the right call to avoid them for a portable CLI; ratatui 0.30 expanded Unicode quadrant/sextant/octant markers, which covers most icon-y needs.
7. **256-color caution.** True-color is the assumption now; only fall back when `COLORTERM` is unset.
8. **"`cargo-dist` will be renamed to `dist` any day now."** Plan documented since 2024, **still hasn't happened on crates.io**. Use `cargo-dist`.

## C. 3 standout apps to study for bee-tui

bee-tui = continuous polling + WebSocket streams + multi-context + cancellable long ops. Best models:

1. **Television** — alexpasmantier/television (https://github.com/alexpasmantier/television). v0.15.0 (Jan 2026). Component-architecture, Elm-flavored. Read `television/src/app.rs` (main loop), `television/src/event.rs` (event aggregation), `television/src/channels/` (multi-source providers — directly analogous to your "contexts"), and `television/src/television.rs` (the `tokio::select!` with throttled render + nucleo background match). This is the cleanest 2026 example of `tokio::select!` over input + tick + render + worker channels.
2. **b4n** — fioletoven/b4n (https://github.com/fioletoven/b4n). k9s-style multi-cluster TUI on ratatui + kube-rs. Look at how it juggles the live `kube::Api` watch streams against the UI loop — a near-exact mirror of your "polling REST + WS push" pattern. Workspace under `b4n-kube/` for the async layer, `b4n-tui/` for the UI.
3. **atuin** — atuinsh/atuin (https://github.com/atuinsh/atuin). Pinned ratatui ^0.30. The `atuin/src/command/client/search/` directory contains a long-running TUI that combines a SQLite query worker + sync HTTP client + key input. CRDT-based sync in 18.12 is worth reading for cancellation/backoff hygiene.

Bonus to skim: **lazyjj** (Cretezy/lazyjj) — small enough to read in one sitting, three-pane TUI, classic Elm-ish dispatch model, directly comparable in shape to bee-tui's planned panes. **gitui** is the canonical component-style example if you want a bigger codebase.

## D. 2026 architecture verdict

**The community has converged but not collapsed to a single answer.** Two patterns dominate; one is now dominant:

- **Component architecture (winner for production multi-pane apps)** — every actively-developed app I checked (television, atuin, gitui, b4n, lazyjj) uses some flavor of: a `Component` trait with `handle_event`, `update`, `draw`; an `App` that owns a `Vec<Box<dyn Component>>`; messages routed through an `mpsc` or via return values. The official `ratatui/templates` repo's **`component` template** is the de-facto starting point. `cargo generate --git https://github.com/ratatui/templates component` is the 2026 "create-react-app" equivalent.
- **Pure Elm (single state, single update) — minority but legitimate** for small/single-screen apps. The async counter tutorial pushes this. Don't pick it for bee-tui (multi-context will outgrow it).
- **`tui-realm`** is no longer the default for new starts.
- **`rat-salsa` 4.x** (thscharler) is the heavyweight contender — built-in event queue, focus management, dialog windows, task spawn. Worth knowing about; pick it only if you need its built-in focus/dialog plumbing.
- **`tokio::select!` directly is still the idiom.** No higher-level abstraction has displaced it; rat-salsa wraps it but most apps drive it directly.

**Recommended for bee-tui:** official `component` template + `tokio::select!` event loop + `tokio_util::sync::CancellationToken` per long-running task, parented to the app token.

## E. Hidden gems that show up in well-built 2026 TUIs

1. **`crossterm::event::EventStream`** with `StreamExt::fuse()` — drop the read-thread, `select!` directly on the stream.
2. **Two intervals, not one.** `tokio::time::interval` for tick (logic) at ~250 ms and a separate one for render at ~16–33 ms (30–60 fps). Television and atuin both do this. Render-rate independent of tick rate is the difference between a snappy and a sluggish TUI.
3. **`tokio_util::sync::CancellationToken` parent/child tree.** App owns a root token; each long task gets a child. Quitting cancels the root and every subscriber; switching context cancels just that subtree. (https://docs.rs/tokio-util/latest/tokio_util/sync/struct.CancellationToken.html)
4. **`tachyonfx` for state transitions, not decoration.** Use `coalesce`/`dissolve` on context switch and `slide_in` on new-data arrival — telegraphs async progress without a spinner.
5. **`color-eyre`'s panic hook integrated with terminal restore** — the template wires it for you. Don't write your own.
6. **`tracing-appender` non-blocking + `RUST_LOG=bee_tui=debug` to a file**, never to stdout (will corrupt the alt-screen). The 2025/2026 `tracing-subscriber` `EnvFilter::builder().from_env_lossy()` is the one-liner.
7. **`insta` with `cargo insta review`** for snapshotting rendered `Buffer`s — `format!("{:?}", buffer)` snapshots are cheap and catch layout regressions.
8. **VHS** (charmbracelet) for visual smoke-tests in CI. Generate `.txt` "golden" output via `Output foo.txt` in the tape; diff in CI. Pairs nicely with insta — no Rust-specific competitor has emerged.
9. **`cargo-binstall` users-side, `cargo-dist` maintainer-side.** Advertise both: `cargo binstall bee-tui` for users with the toolchain, plus the cargo-dist installer script and prebuilt archives for everyone else.
10. **Ratzilla as a free win** — same ratatui code, web preview at `/demo/`. Useful for marketing pages and remote-debugging UIs.
11. **`nucleo` as a generic ranker**, not just a fuzzy finder — bee-tui search across batches/feeds/contexts can reuse Television's pattern.
12. **`ratatui-image`** (separate crate, ratatui org) — if bee-tui ever shows chunk previews / QR codes for shareable refs, it covers Sixel/Kitty/iTerm protocols.

## Things that don't exist or are renamed

- **`lazysql`** — renamed to **Sqlitex**; not the jesseduffield-style TUI you might be imagining.
- **`lazyk8s`** — doesn't exist as a notable Rust TUI. Closest equivalents in ratatui-land: **b4n**, **kubetui**, **kftui**.
- **`lazyssh`** — no popular ratatui project under this name as of May 2026.
- **`ratatui-style` themes/registry** — no community-standard registry has emerged. Themes are still per-app `Style` consts.
- **`dist` (the cargo-dist rename)** — the rename is documented in the book but the published crate is still `cargo-dist 0.31.0`.

## Sources

- [ratatui 0.30 release notes](https://github.com/ratatui/ratatui/releases) / [v0.30 highlights](https://ratatui.rs/highlights/v030/)
- [ratatui templates repo](https://github.com/ratatui/templates) / [Component template docs](https://ratatui.rs/templates/component/)
- [Async event stream tutorial](https://ratatui.rs/tutorials/counter-async-app/async-event-stream/)
- [tachyonfx (now under ratatui org)](https://github.com/ratatui/tachyonfx)
- [Television](https://github.com/alexpasmantier/television)
- [Atuin](https://github.com/atuinsh/atuin)
- [b4n — k9s-style on ratatui](https://github.com/fioletoven/b4n)
- [lazyjj](https://github.com/Cretezy/lazyjj)
- [rat-salsa](https://github.com/thscharler/rat-salsa)
- [Ratzilla (web backend)](https://github.com/ratatui/ratzilla)
- [cargo-dist book](https://axodotdev.github.io/cargo-dist/) / [cargo-binstall](https://github.com/cargo-bins/cargo-binstall)
- [tokio-util CancellationToken](https://docs.rs/tokio-util/latest/tokio_util/sync/struct.CancellationToken.html)
- [VHS](https://github.com/charmbracelet/vhs)
- [awesome-ratatui](https://github.com/ratatui/awesome-ratatui)
