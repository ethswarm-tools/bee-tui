# Keymap cheatsheet

Every key the cockpit handles, in one place. The in-app `?`
overlay is the canonical source — this page mirrors it for
offline reference.

## Global (works everywhere)

| Key | Effect |
|---|---|
| `Tab` | Next screen |
| `Shift+Tab` | Previous screen |
| `[` / `]` | Previous / next tab on the bottom log pane (Errors / Warn / Info / Debug / Bee HTTP / bee::http / Cockpit). Persisted across launches. |
| `+` / `-` | Grow / shrink the bottom log pane height by one line. Clamped to 4..24. Persisted across launches. |
| `Shift+↑` / `Shift+↓` | Scroll the active log tab back / forward by one line. Pauses auto-tail; the title shows a `paused N ↑` indicator. |
| `Shift+PgUp` / `Shift+PgDn` | Same, ten lines at a time. |
| `Shift+End` | Resume auto-tail (snap back to the latest entries). |
| `?` | Toggle help overlay |
| `:` | Open command bar |
| `qq` | Quit — double-tap within ~1.5 s. First `q` shows a footer hint; second `q` confirms. `:q` also works for an unguarded quit. |
| `Ctrl+C` / `Ctrl+D` | Quit immediately. Escape hatch if the cockpit ever stops responding to `qq`. |
| `Esc` | Close help / drill / command bar / cancel current input. Also cancels a pending `q` (so you can back out without committing). |

## Screen-specific keys

S1 / S3 / S5 / S7 / S8 are read-only — they have no
screen-specific keys.

### S2 — Stamps + bucket drill

| Key | Effect |
|---|---|
| `↑↓` / `j k` | Move row selection |
| `↵` | Drill into selected batch (bucket histogram + worst-N) |
| `Esc` | Close drill |

### S4 — Lottery + rchash

| Key | Effect |
|---|---|
| `r` | Fire / re-fire rchash benchmark |

### S6 — Peers + bin saturation + drill

| Key | Effect |
|---|---|
| `↑↓` / `j k` | Move cursor in peer table |
| `PgUp` / `PgDn` | Page through peers |
| `Home` | Jump to first peer |
| `↵` | Drill into selected peer (4 endpoints in parallel) |
| `Esc` | Close drill |

### S9 — Tags / uploads

| Key | Effect |
|---|---|
| `↑↓` / `j k` | Scroll one row |
| `PgUp` / `PgDn` | Scroll ten rows |
| `Home` | Back to top |

### S11 — Pins

| Key | Effect |
|---|---|
| `↑↓` / `j k` | Move cursor through the pinned-reference list |
| `↵` | Drill into selected pin (pin detail) |
| `Esc` | Close drill |

### S12 — Manifests

| Key | Effect |
|---|---|
| `↑↓` / `j k` | Move cursor through the Mantaray tree |
| `↵` | Toggle expand / load the cursored fork (lazy fetch) |

The cursored row's reference (target hex, or fork self-address)
is rendered on a `selected:` detail line above the footer for
terminal-native click-drag copy — there's no explicit copy key.

### S13 — Watchlist

| Key | Effect |
|---|---|
| `↑↓` / `j k` | Move cursor through `:watch-ref` daemons |

### S14 — Feed Timeline

| Key | Effect |
|---|---|
| `↑↓` / `j k` | Move cursor through the feed update history |
| `PgUp` / `PgDn` | Page ten entries |

### S15 — Pubsub watch

| Key | Effect |
|---|---|
| `↑↓` / `j k` | Move cursor through the merged PSS / GSOC timeline |
| `PgUp` / `PgDn` | Page ten entries |
| `c` | Clear the timeline (subscriptions stay open) |

## The command bar

`:` opens it. Once open:

| Key | Effect |
|---|---|
| `↵` | Run the typed command |
| `Esc` | Close without running |
| `Backspace` | Delete left |
| any printable | Append to command buffer |

See [The `:command` bar](../commands/bar.md) for what each
command does.

## Conventions

- The cockpit prefers **vim-style** keys (`j`/`k`,
  `:command`, `Esc`-to-close) but every nav key has an
  arrow-key + named-key alias. You don't have to know vim.
- **No `Ctrl+` chords** for normal navigation. The cockpit
  reserves Ctrl-keys for terminal escape sequences (Ctrl+C
  exits via SIGINT, etc.). All screen actions are single
  keystrokes.
- **`Esc` is universal close.** Whatever's most-recently
  opened — drill / help / command bar — is what `Esc`
  closes. The hierarchy is: command bar > help overlay >
  drill > nothing.

## Why `qq` instead of just `q`

A bee-tui session is something operators leave running in the
background while doing other work. A single `q` was found to
be too easy to misclick — especially when navigating in from
another shell. The double-tap guard means a stray keystroke
costs you a footer hint, not a session.

If you really want unguarded quit, use `:q` from the command
bar. `Ctrl+C` and `Ctrl+D` are also unguarded — they remain
the canonical "I want out *now*" escape hatches and bypass
the double-tap entirely.

## Discovering keys

Open `?` on any screen. The overlay shows the global keymap
*plus* the keys for the current screen. So pressing `?` on
S6 lists peer-drill keys; pressing `?` on S9 lists scroll
keys. No memorisation needed.

## What's not bound

The cockpit deliberately leaves these unbound:

- **Up/down arrow for screen jump** — `Tab` is the only
  screen-jump key. Arrow keys are reserved for in-screen
  navigation.
- **Number keys for screen jump** — would conflict with
  future selection / drill operations.
- **`/` for search** — there's no global text search yet.
  Most screens are too short to need one, and where they
  aren't (S6 peers, S9 tags), you can scroll with
  `j`/`k`/`PgDn`/`Home`.
