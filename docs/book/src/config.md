# Configuration

bee-tui's configuration is a single TOML file. With no config
at all, the cockpit talks to `http://localhost:1633` against
a node with no auth token — the most common dev setup. As
soon as you have a real Bee node with a Bearer token, or
multiple nodes you want to switch between, you'll want a
config.

## Where the config lives

bee-tui looks for `config.toml` in this order, taking the
first hit:

1. The path in the `BEE_TUI_CONFIG` environment variable, if set
2. `$XDG_CONFIG_HOME/bee-tui/config.toml`
3. `~/.config/bee-tui/config.toml`
4. (built-in default — single `local` node, no token)

The directory does **not** need to exist before launch;
bee-tui only reads, never writes. Create it yourself the
first time:

```sh
mkdir -p ~/.config/bee-tui
$EDITOR ~/.config/bee-tui/config.toml
```

## Minimal example

```toml
[[nodes]]
name    = "prod-1"
url     = "http://10.0.1.5:1633"
token   = "@env:BEE_TOKEN_PROD1"
default = true
```

That's the whole file: one node, named `prod-1`, with its
auth token resolved from `$BEE_TOKEN_PROD1` at startup.

## Schema reference

### `[[nodes]]` — the node array

You can declare any number of `[[nodes]]` entries. Exactly
one should have `default = true`; that's the profile bee-tui
loads on launch. The others are reachable via `:context <name>`.

| Field | Type | Required | Description |
|---|---|---|---|
| `name` | string | yes | Identifier shown in the top bar and used by `:context <name>`. Keep short — `prod-1`, `lab`, `staging`. |
| `url` | string | yes | Base URL of the Bee node, e.g. `http://localhost:1633` or `https://bee.example.com:1633`. Trailing slash optional. |
| `token` | string | no | Bearer token. May be the literal token string, or `@env:VAR_NAME` to resolve from an environment variable at startup. Empty / missing = no auth header sent. |
| `default` | bool | no | If true, this profile is loaded on launch. Exactly one entry should have it. |

### `[ui]` — UI preferences

```toml
[ui]
theme           = "default"
ascii_fallback  = false
```

| Field | Type | Default | Description |
|---|---|---|---|
| `theme` | `"default"` \| `"mono"` | `"default"` | Slot-based palette. `default` is vibrant green/yellow/red. `mono` is greyscale only — useful on terminals where colour is muted or distracting, or when piping to a recording tool that doesn't preserve colour. |
| `ascii_fallback` | bool | `false` | If true, every component renders ASCII glyphs (`OK / X / ! / > / # / .`) instead of Unicode (`✓ ⚠ ✗ ▶ ▇ ░`). Equivalent to passing `--ascii` on the command line. Use on Windows Terminal pre-Win11, screen readers, or SSH chains that mangle Unicode. |

Unknown values for `theme` fall back to `"default"` with a
single tracing warning so a typo doesn't break startup.

### CLI overrides

Three command-line flags override the config file:

```sh
bee-tui --ascii        # forces ascii_fallback = true
bee-tui --no-color     # forces theme = "mono"
NO_COLOR=1 bee-tui     # same as --no-color, per <https://no-color.org>
```

Resolution order (highest priority first):

1. `--ascii` flag → ascii glyphs (regardless of config)
2. `--no-color` flag OR `NO_COLOR` env (any non-empty value) → mono palette
3. `[ui].ascii_fallback` from config → ascii glyphs
4. `[ui].theme` from config → palette

## The `@env:VAR` token form

Every Bee API endpoint that's not explicitly public requires
a Bearer token. Hard-coding the token in `config.toml` is
fine for a lab node, but for production it's the wrong shape —
the file lands in dotfiles backups, screenshots, support
threads, etc. The `@env:VAR` form keeps the token out of
the file:

```toml
token = "@env:BEE_TOKEN_PROD1"
```

bee-tui reads `$BEE_TOKEN_PROD1` **once at startup** and uses
the resolved value for every request. The literal string
`@env:BEE_TOKEN_PROD1` is never logged, never captured in
`:diagnose` bundles, never sent to Bee. If the variable is
unset, bee-tui logs a tracing warning and proceeds without
an auth header (the request will then 401).

You can mix forms across nodes — one `@env:` and one literal
in the same config is fine.

## Multi-node setups

```toml
[[nodes]]
name    = "prod-1"
url     = "http://10.0.1.5:1633"
token   = "@env:BEE_TOKEN_PROD1"
default = true

[[nodes]]
name  = "prod-2"
url   = "http://10.0.1.6:1633"
token = "@env:BEE_TOKEN_PROD2"

[[nodes]]
name = "lab"
url  = "http://localhost:1633"

[ui]
theme = "default"
```

Launch picks `prod-1` (the default). At runtime, switch with:

- `:context prod-2` — swap to the second prod node
- `:context lab` — swap to the local lab node
- `:context` — list every configured profile name

The switch is fast (no restart) but **not stateful across
launches** — every run starts on the `default = true`
profile.

See [`:context`](./commands/context.md) for the deep dive
on what's preserved vs reset on switch.

## Validating your config

If bee-tui fails to start with a config error, the message is
the first thing on stderr — common ones:

| Error | Fix |
|---|---|
| `no Bee node configured (config.nodes is empty)` | Add at least one `[[nodes]]` entry. |
| `no default node selected` | Mark exactly one `[[nodes]]` with `default = true`. |
| `invalid url: …` | Quote URLs that contain ports: `url = "http://10.0.1.5:1633"` (TOML accepts unquoted in a TOML number-shape unrelated to URLs, leading to confusing parses). |
| `unknown theme name "X" — falling back to default` | Just a warning; not fatal. Set `theme` to `"default"` or `"mono"`. |

To dump the resolved config for debugging:

```sh
:diagnose
```

The bundle in `$TMPDIR/bee-tui-diagnostic-<ts>.txt` includes
the active profile name and endpoint URL. Tokens are never
captured — they live in HTTP headers, not URLs.
