# Theme & accessibility

The cockpit ships two themes (`default`, `mono`) and a
glyph fallback (`ascii`) so it works on terminals that don't
render Unicode or colour cleanly — colourblind operators,
screen readers, recording tools, SSH chains that mangle
terminal escapes, Windows pre-Win11.

## Themes

### `default` (vibrant)

The default. Status uses semantic colour:

- Green for Pass / healthy
- Yellow for Warn / in-progress
- Red for Fail / critical
- Blue for Info / accent
- Dim grey for Unknown / muted

This is what most operators see. It works on every modern
terminal (iTerm2, Alacritty, Kitty, Wezterm, Konsole, GNOME
Terminal, Windows Terminal on Win11+).

### `mono` (greyscale)

Same layout, no colour. Status is conveyed *only* through
glyphs and intensity. Useful when:

- Recording the terminal to a video / GIF (colour-corrupting
  recorders preserve glyphs)
- Piping through tools that strip ANSI colour
- The terminal's colour palette is corrupted by a theme
  override
- Personal preference for a calm aesthetic

Set in config:

```toml
[ui]
theme = "mono"
```

Or via CLI:

```sh
bee-tui --no-color
NO_COLOR=1 bee-tui    # equivalent — per <https://no-color.org>
```

The `NO_COLOR` environment variable is the standard
cross-tool convention; the cockpit honours it to slot into
existing setups without per-tool config.

## ASCII fallback

Independent from the theme. Replaces Unicode glyphs with
ASCII equivalents:

| Unicode | ASCII | Used for |
|---|---|---|
| `✓` | `OK` | Pass status |
| `⚠` | `!` | Warn status |
| `✗` | `X` | Fail status |
| `·` | `.` | Unknown / dim |
| `▶` | `>` | Selection cursor |
| `▇` | `#` | Filled bar segment |
| `░` | `-` | Empty bar segment |
| `▒` | `=` | In-progress fill |
| `⏳` | `*` | Pending |
| `└─` | `|-` | Tooltip continuation |
| `—` | `-` | Em dash / "never" |

Set in config:

```toml
[ui]
ascii_fallback = true
```

Or via CLI:

```sh
bee-tui --ascii
```

When to use `ascii`:

- Older Windows Terminal (pre-Win11) renders most Unicode
  glyphs as `?`
- Screen readers — Unicode geometric shapes are read aloud
  unpredictably; ASCII letters are stable
- SSH through a non-UTF8 terminal multiplexer
- Some vim integrations / tmux configurations

## Resolution order

Multiple sources can configure theme + glyphs. The
resolution is (highest priority first):

1. `--ascii` flag → ASCII glyphs (regardless of config)
2. `--no-color` flag OR `NO_COLOR` env (any non-empty value) → mono palette
3. `[ui].ascii_fallback = true` from config → ASCII glyphs
4. `[ui].theme = "..."` from config → palette
5. Built-in defaults: `default` theme, Unicode glyphs

## Accessibility checklist

The cockpit's design rules:

- **Status is conveyed redundantly.** Every Pass / Warn /
  Fail uses *both* a glyph (✓ / ⚠ / ✗ or OK / ! / X) *and*
  a colour. No information is lost in mono or under ASCII.
- **No flashing or animation tied to status.** The only
  movement is the spinner glyph for cold-start "loading…"
  rows, and that's a 4-frame cycle at low frequency.
- **No keystrokes require modifier keys** for navigation.
  `Tab`, `j`/`k`, `↑↓`, `Esc`, `↵`, `?` — every primary
  action is a single key.
- **Focus is single-screen at a time.** No tabbing between
  panes within a screen; the whole screen is the unit.

## Slot-based palette

For developers — the theme system uses *slots* (semantic
roles) rather than direct colour assignments:

| Slot | Default | Mono | What it carries |
|---|---|---|---|
| `pass` | green | white | Healthy / success |
| `warn` | yellow | white-dim | In-progress / cautionary |
| `fail` | red | white-dim | Failure / critical |
| `info` | blue | white | Accent / informational |
| `accent` | magenta | white | Headers / titles |
| `dim` | grey | grey | Muted / unknown |
| `text` | white | white | Body text |

Components reference slots (`theme::active().fail`), never
raw colours. Adding a new theme is a matter of mapping the
slots to a new palette — see
[Adding a screen](../internals/add-screen.md) for the
extension hook.

## Glyph slots

Same idea, for symbols. 12 slots:

```
pass, warn, fail, bullet (·), spinner (4 frames),
selection (▶), bar_filled (▇), bar_empty (░),
bar_partial (▒), pending (⏳), continuation (└─),
em_dash (—)
```

The Unicode and ASCII variants are constructed via
`Glyphs::unicode()` and `Glyphs::ascii()`. Code that wants
to detect the active mode does it with a content equality
check (`active().glyphs.pass == Glyphs::unicode().pass`)
since pointer equality on string literals isn't reliable
across optimisation boundaries.

## Reporting accessibility bugs

If a screen is unreadable in mono / ASCII / a specific
terminal, file an issue with:

- Terminal + version (`echo $TERM` output is helpful)
- The cockpit invocation (with what flags)
- A screenshot or copy of the rendered output

We treat accessibility bugs as P1 — the cockpit is for
operators, and operators don't always work on a colour-
capable Linux laptop.
