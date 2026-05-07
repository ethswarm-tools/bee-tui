# The `:command` bar

A vim-style colon prompt for actions that don't fit on the
keymap: jump to a screen by name, fire on-demand checks,
switch profiles, export a diagnostic bundle.

## Opening + closing

| Key | Effect |
|---|---|
| `:` | Open the command bar (focus moves to a one-line prompt at the bottom) |
| `Esc` | Close without running |
| `↵` | Run the command |
| `Backspace` | Delete left |

The screen behind the bar keeps refreshing — gauges don't
freeze while you're typing.

## Status line

After a command runs, the bottom line shows the result for
~3 seconds before fading:

- **Info** (green) — `→ Health`, `diagnostic bundle exported to /tmp/...`
- **Err** (red) — `unknown command: "..."`, `usage: :set-logger <expr> <level> ...`

If you missed the message, just re-run — the status sticks
until the next command or the next 3 s tick.

## Screen jumps

Every screen has a name; `:<name>` jumps there.

| Command | Screen |
|---|---|
| `:health` | S1 — Health gates |
| `:stamps` | S2 — Stamps + bucket drill |
| `:swap` | S3 — SWAP / cheques |
| `:lottery` | S4 — Lottery + rchash |
| `:warmup` | S5 — Warmup checklist |
| `:peers` | S6 — Peers + bin saturation |
| `:network` | S7 — Network / NAT |
| `:api` | S8 — RPC / API health |
| `:tags` | S9 — Tags / uploads |
| `:log` | S10 — Command log |

These are equivalent to pressing `Tab` until you reach the
target screen, but faster on a 10-screen carousel.

## Action commands

| Command | Page | What it does |
|---|---|---|
| `:diagnose` (alias `:diag`) | [diagnose](./diagnose.md) | Dump the full snapshot + recent log buffer to a file |
| `:pins-check` (alias `:pins`) | [pins-check](./pins-check.md) | Run a full integrity check on every locally pinned reference |
| `:loggers` | [loggers](./loggers.md) | Snapshot the live logger registry to a file |
| `:set-logger <expr> <level>` | [loggers](./loggers.md) | Change one logger's verbosity at runtime |
| `:context <name>` (alias `:ctx`) | [context](./context.md) | Switch to a different node profile from your config |
| `:context` | [context](./context.md) | List configured profiles (no switch) |
| `:quit` (alias `:q`) | — | Exit the cockpit |

## Why a colon prompt?

Two reasons:

1. **Discoverability without clutter.** The cockpit can have
   ten screen-jumps + half a dozen action commands without
   each one needing its own keybinding. The keymap stays
   minimal (`Tab`, `↵`, `Esc`, `?`, `:`, `q`); rare commands
   live behind the colon.
2. **Familiarity.** Anyone who's used vim, k9s, or lazygit
   has the muscle memory. The cockpit's job is to *not*
   require new muscle memory.

## What's not on the bar

These actions deliberately don't have a `:command` form:

- **Cashing out cheques.** Cashout is on-chain; it costs gas;
  you should think about whether to do it. The cockpit
  surfaces the data (S3 Pane 2) but won't trigger the
  on-chain transaction. Use `curl POST /chequebook/cashout/<peer>`
  if you really mean it.
- **Buying / topping up postage.** Same reasoning. S2 shows
  TTL and worst-bucket, but `bee postage buy` and `bee
  postage topup` are operator decisions with funding
  consequences.
- **Stake deposit / withdraw.** Same.
- **Connect / disconnect peers.** Bee's kademlia handles
  this without operator help; manual `connect` is a
  debugging escape hatch.

The cockpit is a read-mostly observer. The few mutating
commands it *does* have (`:set-logger`) are scoped to
diagnostic state, not funds-bearing actions.

## See also

- [`:diagnose`](./diagnose.md)
- [`:pins-check`](./pins-check.md)
- [`:loggers` / `:set-logger`](./loggers.md)
- [`:context`](./context.md)
