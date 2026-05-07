# bee-tui architecture brief — production Ratatui patterns

Studied: **gitui** (extrawurst/gitui, master), **atuin** (atuinsh/atuin, main), **bottom** (ClementTsang/bottom), **gpg-tui** (orhun/gpg-tui).

## What the production apps actually do

| Concern | gitui | atuin | gpg-tui |
|---|---|---|---|
| Runtime | sync threads + `crossbeam-channel` (no tokio) | tokio + `tokio::select!` | sync polling loop |
| Channels | 6 typed `Receiver<T>` multiplexed via `crossbeam::Select` (`src/main.rs` `select_event`) | none — `tokio::spawn` + `.fuse()` futures merged in select | none |
| Tick / FPS | `TICK_INTERVAL=5s`, `SPINNER_INTERVAL=80ms` (gitui `src/main.rs` 217–218) | `event::poll(250ms)` blocking poll = implicit 4 fps cap | fixed `args.tick_rate` |
| Cancellation | `AsyncGitNotification` discriminated by job hash; results from stale jobs discarded | implicit — re-run query, drop old future via scope exit | n/a |
| State | God-`App` with 25+ component fields + `Environment{queue,theme,key_config,sender_*}` injected by ref | single `State` struct, all mutations via `execute_action(Action)` | single `App`, mutated through `&mut app` |
| Components | `Component` trait: `event(&mut, &Event)->Result<EventState>` + `commands()->CommandBlocking` + `focused()`; separate `DrawableComponent::draw(&self, &mut Frame, Rect)` (gitui `src/components/mod.rs`) | none — monolithic match in `interactive.rs` | none |
| Input | `event_pump` walks `components_mut()`, first `EventState::Consumed` wins; `SharedKeyConfig: Rc<RefCell<_>>` | `KeymapSet` field on `State`, `handle_input` returns `InputAction` | per-handler |
| Panic | `panic::set_hook` calls `shutdown_terminal()` then logs (gitui `src/main.rs` 276–279) + `defer!` macro for normal exit | none in the file I read — must be in caller | none in main |
| Errors | `anyhow` | `eyre` (not color-eyre) | — |
| Config | RON via `ron = "0.12"` | TOML via `config = "0.15"` | — |
| Snapshot tests | `insta = "1.41"` with `filters` feature | `pretty_assertions` only | — |
| MSRV | 1.88 | 1.95 | — |

## Recommended architecture for bee-tui

**Hybrid: Elm-style core + gitui's `Component` trait for screens.** Pure Elm gets unwieldy past ~5 screens (atuin's `execute_action` is already strained); pure components fragment ownership. Combine them.

> **Plan v2 update:** Subsequent 2026 SOTA research found the community converged on the official `ratatui/templates` `component` template. v2 plan adopts that template directly instead of the hybrid here.

**Modules (decide now, don't bikeshed later):**
```
src/
  main.rs              // tokio::main, panic hook, terminal lifecycle
  app.rs               // App, run_loop, tokio::select! over channels
  event.rs             // Event enum, EventBus
  message.rs           // Msg enum (input + async results), update()
  components/
    mod.rs             // Component trait (gitui-style)
    nodes/             // multi-node list + drill-down
    chunks/            // chunk upload/download
    feeds/             // feed inspector
    stamps/            // postage batches
    debug_api/         // /addresses, /topology, /peers
    overlay/           // help, command bar, confirm
  api/                 // bee REST + debug clients (reqwest)
  state/               // typed snapshots (TopologySnapshot, etc.)
  config.rs            // figment + TOML; theme, keys, endpoints
  theme.rs
  tracing.rs           // tracing-subscriber + file appender
```

**Channels (name them):**
- `tx_msg: tokio::sync::mpsc::UnboundedSender<Msg>` — single ingest. Input task, API tasks, ticker all push `Msg`.
- `tokens: HashMap<ScreenId, CancellationToken>` — one per screen; switching screens calls `tokens[old].cancel()` (this is what gitui *should* have but works around with hash discrimination).
- `tokio::sync::watch::<Snapshot>` per long-poll stream (topology, balances) — last-value semantics, no backpressure needed.
- No broadcast. No crossbeam — gitui only uses it because they predate tokio adoption.

**Frame budget:** redraw on `Msg` arrival, but coalesce with `tokio::time::sleep_until` to a 60ms floor (~16 fps). Idle ticker every 1s, not 5s like gitui — Bee state changes faster than git.

> **Plan v2 update:** Switched to two intervals — tick 250ms (logic) + render 16-33ms (60fps). Atuin and Television both do this. Render-rate independent of tick rate is the difference between snappy and sluggish.

## 6 patterns to copy verbatim

1. **gitui `Environment` injection** (`src/app.rs`): bundle `theme`, `key_config`, `tx_msg`, `api_client` into one struct, pass `&Environment` to every `Component::new`. Avoids prop-drilling and the god-struct trap.
2. **gitui `EventState::{Consumed, NotConsumed}` + `event_pump`** — the cleanest input-routing pattern in Rust TUI land. Steal the trait wholesale.
3. **gitui dual panic strategy** (`src/main.rs` 276 + `defer!` 193): set panic hook *and* RAII guard. `color-eyre` alone is not enough — its hook fires but doesn't disable raw mode.
4. **atuin's input batching** (`while event::poll(Duration::ZERO)?` drain loop) — prevents UI freeze when scrolling fast.
5. **gitui's `Queue` + `InternalEvent` enum** — popups don't call siblings directly, they push `InternalEvent::ShowConfirm(...)`. Decouples popups from screens.
6. **bottom's `inspect_err(reset_stdout)`** pattern at the entrypoint as a final safety net even with the panic hook.

## 3 traps to avoid

1. **gitui's god `App` struct with 25+ popup fields.** Use `popup_stack: Vec<Box<dyn Component>>` instead — same effect, no `accessors!` macro needed.
2. **atuin re-running the search on every keystroke without cancellation.** With Bee's 30–60s feed lookups, this would queue requests forever. Use `CancellationToken` per screen.
3. **gpg-tui's blocking `tui.events.next()`** — fine for gpg, fatal for Bee where you want streaming `/topology` updates. Always `tokio::select!` over input + API + tick.

## Library BoM (2026 standard)

**Core:** `ratatui = "0.30"` (default-features = false, features = ["crossterm"]) · `crossterm = "0.29"` · `tokio = { version = "1", features = ["full"] }` · `tokio-util` (CancellationToken).

**Errors / panics:** `color-eyre = "0.6"` (preferred over anyhow for TUIs — pretty backtraces; gitui sticks with anyhow only because they predate it).

**Logging:** `tracing` + `tracing-subscriber` + `tracing-appender` (file rotation — atuin uses tracing). Skip `log`/`simplelog`.

**Config:** `figment = "0.10"` (TOML + env layers) over the `config` crate — better diagnostics. `directories = "6"` for XDG paths. `serde` + `toml`.

> **Plan v2 update:** 2026 SOTA research showed `figment` stagnating (last release 2024-05); `config 0.15` is now the active default. v2 plan uses `config`.

**Input widgets:** `tui-input = "0.14"` for command bar + endpoint forms. `tui-textarea = "0.7"` only if you add a payload editor — heavier. **Skip** `ratatui-textarea` (gitui's pin is stale).

**Maps & collections:** `indexmap` (stable iteration for peer lists), `dashmap` only if you share state across tasks without `Arc<Mutex>` — usually you don't need it; one `mpsc` is cleaner.

**Visual polish:** `throbber-widgets-tui` (spinners during chunk uploads), `tui-popup = "0.6"` (modal dialogs), `tui-tree-widget` (manifest/Mantaray browser — directly relevant to Bee). **Skip** `tachyonfx` unless you want effects; it adds 200kb.

> **Plan v2 update:** `tachyonfx 0.25` is now under the `ratatui/` org; use it for state-transition effects (coalesce/dissolve on context switch, slide_in on new-data arrival), not decoration.

**Testing:** `insta` (snapshot tests against `Buffer` — gitui pins 1.41). `wiremock` for fake Bee responses. `assert_cmd` for binary smoke tests.

**Distribution:** `cargo-dist` (replaces hand-rolled matrices; produces homebrew + scoop + msi). MSRV: pin **1.85** to match bee-rs — newer is fine, but matching keeps one toolchain.

## Verification refs

- gitui main loop: `extrawurst/gitui` `src/main.rs` lines ~217, 247–262, 276–279
- gitui App: `src/app.rs` (search `pub struct App`, `pub struct Environment`, `event_pump`, `accessors!`)
- gitui Component: `src/components/mod.rs` (`trait Component`, `EventState`, `DrawableComponent`)
- gitui Cargo: `Cargo.toml` (ratatui 0.30, anyhow, ron 0.12, insta 1.41, rust-version 1.88)
- atuin loop: `crates/atuin/src/command/client/search/interactive.rs` (`tokio::select!`, `event::poll(250ms)`, `query_results`, `State` struct)
- atuin Cargo: `Cargo.toml` (ratatui 0.30, eyre 0.6, config 0.15, tracing 0.1, MSRV 1.95)
- bottom entry: `src/bin/main.rs` (`inspect_err(reset_stdout)` pattern)
- gpg-tui main: `orhun/gpg-tui` `src/main.rs` (`while app.state.running` blocking loop — *don't* copy)
