# Keymap cheatsheet

Every key the cockpit handles, in one place. The in-app `?`
overlay is the canonical source — this page mirrors it for
offline reference.

## Global (works everywhere)

| Key | Effect |
|---|---|
| `Tab` | Next screen |
| `Shift+Tab` | Previous screen |
| `?` | Toggle help overlay |
| `:` | Open command bar |
| `q` | Quit |
| `Esc` | Close help / drill / command bar / cancel current input |

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
