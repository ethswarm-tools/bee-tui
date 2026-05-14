# Launching bee-tui

Every way to start the cockpit, in one place. The short
version: run `bee-tui` with no arguments and it connects to
`http://localhost:1633`. Everything below is for pointing it
somewhere else, connecting to several nodes at once, or
changing how it runs.

For the deep config-file schema (tokens, `[ui]`, `[alerts]`,
`[bee]`, …) see [Configuration](./config.md). This page is
about *invocation*.

## The five ways to pick which node(s) you connect to

bee-tui resolves its node list from the first of these that
applies:

| # | How | Example |
|---|---|---|
| 1 | **Positional URLs** (v1.16+) | `bee-tui http://localhost:1633 https://bee-eu.example.com:1633` |
| 2 | **`--config <file>`** (v1.15.2+) | `bee-tui --config ~/work/bee-nodes.toml` |
| 3 | **`$BEE_TUI_CONFIG` directory** | `BEE_TUI_CONFIG=~/.config/bee-tui bee-tui` |
| 4 | **Config-file discovery** | `bee-tui` (finds `~/.config/bee-tui/config.toml`) |
| 5 | **Built-in default** | `bee-tui` (no config anywhere → `local` @ `localhost:1633`) |

Positional URLs win over everything: when you pass URLs, the
config file's `[[nodes]]` list is ignored (the rest of the
file — `[ui]`, `[alerts]`, … — still applies). `--config` and
`$BEE_TUI_CONFIG` and discovery are mutually the "where's the
config file" question; see [Configuration](./config.md#where-the-config-lives)
for that search order.

### 1. Default — no arguments

```sh
bee-tui
```

With no config file anywhere on the search path, this talks to
`http://localhost:1633` with no auth token — the standard local
dev setup. With a config file, it loads the `default = true`
profile from it.

### 2. Node URLs on the command line (v1.16+)

```sh
bee-tui http://localhost:1633
bee-tui http://localhost:1633 https://bee-eu.example.com:1633 https://bee-us.example.com:1633
bee-tui localhost:1633          # scheme-less → treated as http://
```

The URLs become an **ad-hoc fleet** for that session, with no
config file needed. The first URL is the active/default node;
all of them appear in the `Ctrl+N` picker and the S15 Fleet
view. Node names are derived from each URL's host
(`bee-eu.example.com` → `bee-eu`; IPs and bare hosts kept
whole), with a `-2` / `-3` suffix on collision.

Command-line URLs **cannot carry a bearer token** — for
restricted-mode nodes use a config file (the `@env:VAR` token
form keeps the secret out of your shell history).

### 3. A specific config file — `--config` (v1.15.2+)

```sh
bee-tui --config ~/work/bee-nodes.toml
bee-tui --config ./staging.yaml
```

Points straight at a config *file*, bypassing the directory
search. The file must exist and end in a recognised extension
(`.toml`, `.json5`, `.json`, `.yaml` / `.yml`, `.ini`); a
missing file or unknown extension fails fast with a clear
message instead of silently falling back.

### 4. A config directory — `$BEE_TUI_CONFIG`

```sh
BEE_TUI_CONFIG=~/.config/bee-tui bee-tui
```

`BEE_TUI_CONFIG` is a **directory** that bee-tui looks *inside*
for `config.toml` — not a file path. (Use `--config` if you
want to name the file directly.)

### 5. Config-file discovery

With none of the above, bee-tui searches a list of directories
and loads the first `config.toml` it finds — `~/.config/bee-tui/`
works on every platform, including macOS and Windows. The full
search order is in
[Configuration → Where the config lives](./config.md#where-the-config-lives).

`bee-tui --version` prints the directory a config was actually
loaded from — the fastest way to confirm what's in effect.

## Letting bee-tui start Bee for you — supervised mode

By default bee-tui connects to an **already-running** Bee. If
you'd rather it spawn and supervise the Bee process itself,
point it at the binary + Bee's YAML config:

```sh
bee-tui --bee-bin ./bee --bee-config ./bee.yaml
```

bee-tui starts Bee as a child process, captures its log,
waits for the API to come up, then opens the cockpit — and
tears Bee down on quit. The equivalent persistent form is a
`[bee]` block in `config.toml` (which also unlocks the
`[bee.supervisor].auto_restart` watchdog). The CLI flags
override the `[bee]` block.

## Showing Bee's logs in the bottom pane

When bee-tui connects to an external Bee (not supervised),
the bottom pane's Bee-side tabs need to know where that Bee
writes its log:

```sh
# Local node: nothing to do — bee-tui auto-discovers the log
bee-tui

# Remote / explicit: a log file, or a command whose stdout is the log
bee-tui --bee-log /var/log/bee/bee.log
bee-tui --bee-log-cmd "journalctl -u bee -f"
bee-tui --bee-log-cmd "ssh bee-host 'tail -f /var/log/bee.log'"
```

`--bee-log` / `--bee-log-cmd` apply to the active node and
override its `[[nodes]].log_file` / `log_command`. They're
ignored in supervised mode (the captured child log is tailed
instead). See [S10 — Bottom log pane](./screens/s10-log.md)
for the full picture.

## Rendering and performance flags

| Flag | Effect |
|---|---|
| `--ascii` | Render with ASCII glyphs only — no Unicode `✓ ⚠ ✗ ▶ ▇`. For terminals with poor Unicode support. Same as `[ui].ascii_fallback = true`. |
| `--no-color` | Suppress colour regardless of theme. Same as `[ui].theme = "mono"`. The `NO_COLOR=1` env var is also honoured automatically per [no-color.org](https://no-color.org). |
| `--tick-rate <FLOAT>` | Ticks per second (default `4.0`) — how often pollers and the spinner advance. |
| `--frame-rate <FLOAT>` | Frames per second (default `60.0`) — redraw cadence. |

## CI / scripting mode — `--once`

`bee-tui --once <verb> [args…]` runs a single verb without
launching the TUI, prints the result on stdout, and exits with
a meaningful status code. This is the path for shell pipelines
and CI smoke tests:

```sh
bee-tui --once readiness                       # "is my node ready for traffic?"
bee-tui --once readiness --json                # machine-readable output
bee-tui --config ./prod.toml --once readiness  # against a specific config
```

In `--once` mode the positional arguments are the **verb's
arguments**, not node URLs. Full details on the
[`--once` CI mode](./commands/once.md) page.

## Inspecting the invocation

```sh
bee-tui --version   # version + the resolved config + data directories
bee-tui --help      # every flag, with descriptions
```

## How it quits

`q` (double-tapped) or `Ctrl+C` quits cleanly: every in-flight
HTTP request is cancelled, the terminal is restored from
alt-screen, and a supervised Bee child is torn down. Nothing is
persisted implicitly — a `:context` switch you made during the
session doesn't stick; the next launch resolves nodes from
scratch using the order at the top of this page.
