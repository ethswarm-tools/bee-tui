# Keymap cheatsheet

Every key the cockpit handles, in one place. The in-app `?`
overlay is the canonical source — this page mirrors it for
offline reference.

## Global (works everywhere)

| Key | Effect |
|---|---|
| `Tab` | Next screen |
| `Shift+Tab` | Previous screen |
| `1` – `9` | Jump to S1 – S9 |
| `0` | Jump to S10 (Pins) |
| `Alt+1` – `Alt+5` | Jump to S11 – S15 (Manifest, Watchlist, FeedTimeline, Pubsub, Fleet) |
| `Ctrl+N` | Open node picker (also `:nodes`) |
| `Ctrl+Alt+N` | Open notification history overlay (v1.14+) — see [`:notifications`](#the-notification-history-overlay) |
| `Shift+E` | Open the batch-economics modal (v1.12+) — guided form for topup/dilute/extend/buy/plan previews |
| `Shift+L` | Toggle fullscreen log pane (v1.13+) — collapses the active screen so the log pane fills the middle of the cockpit. Press again to return. |
| `/` | Open the log-pane filter prompt (v1.13+) — case-insensitive substring; `Enter` commits, `Esc` cancels. With an active filter, `Esc` (no prompt open) clears it. |
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

## The node picker overlay

`Ctrl+N` (or `:nodes`) opens a centred overlay listing every
`[[nodes]]` entry from `config.toml`. The cursor lands on the
currently active node:

| Key | Effect |
|---|---|
| `↑↓` / `j k` | Move cursor through configured nodes |
| `↵` | Switch to the cursored node (rebuilds API client + watch hub; no-op if cursor is already on the active node) |
| `Esc` / `Ctrl+N` | Close without switching |

The active node is marked `●` and the `default = true` entry is
marked `★`. After switching, the metadata line at the top of the
cockpit updates to show the new profile and endpoint; any
`:watch-ref` daemons and pubsub subscriptions that were running
against the previous node are cancelled (they don't follow the
context — re-issue the verbs against the new node if you want them
there too).

## The notification history overlay

`Ctrl+Alt+N` (or `:notifications`) opens a centred overlay listing the
most recent 200 alert transitions the cockpit has observed, newest
first. Same source as the toast overlay (and the optional
webhook / desktop notification sinks): every gate transition (S1
health gates + S15 fleet rows) becomes one entry, severity-coloured.

| Key | Effect |
|---|---|
| `Esc` | Close |

Toasts appear in the top-right corner whenever a new transition is
ingested. They auto-dismiss after `[notifications].toast_seconds`
(default 8 s) and are capped at 3 visible at once. Disable them by
setting `[notifications].toast_enabled = false` in `config.toml` —
the history overlay still works.

## The help overlay

`?` opens a centred overlay with two pages:

| Key | Effect |
|---|---|
| `?` | Toggle the overlay |
| `Tab` / `Shift+Tab` | Switch between **Keys** and **Verbs** pages |
| `Esc` / `?` / `q` | Close |

The **Keys** page mirrors this cheatsheet (global keys + the
screen-specific block for whichever screen is active). The
**Verbs** page lists every `:verb` grouped by category (navigate,
inspect, stamps & economics, uploads, durability, pubsub, mining,
diagnostics, cockpit) so the entire surface is discoverable
without leaving the cockpit.

## What's not bound

The cockpit deliberately leaves these unbound:

- **Up/down arrow for screen jump** — `Tab` (or the digit keys)
  is the screen-jump path. Arrow keys are reserved for in-screen
  navigation.
- **`/` for search** — there's no global text search yet.
  Most screens are too short to need one, and where they
  aren't (S6 peers, S9 tags), you can scroll with
  `j`/`k`/`PgDn`/`Home`.
