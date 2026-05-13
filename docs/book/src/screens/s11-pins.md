# S10 — Pins

> Earlier docs (and the file name `s11-pins.md`) called this S11.
> The screen is now the **10th** tab in the strip — the file
> name is kept for stable links.

Sortable list of every reference Bee has pinned locally, with
on-demand integrity checks. Promotes the
[`:pins-check`](../commands/pins-check.md) command's
write-to-file output into a real screen so operators can browse
their pin set, spot the unhealthy ones, and re-check a single
pin without walking the whole graph.

## Columns

| Column | What it shows |
|---|---|
| `REFERENCE` | The 32-byte pin reference, shortened to `prefix…suffix` form |
| `TOTAL` | Total chunks reachable from the pin (after a check; `—` until then) |
| `MISSING` | Chunks that should be reachable but are missing locally |
| `INVALID` | Chunks present but failing integrity validation |
| `STATUS` | One of `? unchecked` / `· checking…` / `✓ healthy` / `✗ degraded` / `✗ check failed: …` |

Header summary: `N pinned   ✓ X   ✗ Y   ? Z   sort <mode>`. The
counts let an operator spot the alert state (red `✗`) without
scanning every row.

## Keymap

| Key | What it does |
|---|---|
| `↑↓` / `j k` | Move row selection |
| `Enter` | Integrity-check the highlighted pin (single `/pins/check?ref=…` call) |
| `c` | Integrity-check every pin currently `unchecked` |
| `s` | Cycle sort: `ref order` → `bad first` → `by size` |

## Sort modes

- **`ref order`** (default) — Bee's response order; matches
  `curl /pins`.
- **`bad first`** — unhealthy → check-failed → unchecked →
  checking → healthy. Surfaces the rows that matter for an
  operator who suspects local chunk loss.
- **`by size`** — largest pin first by `total` chunk count.
  Useful when figuring out which pin set dominates local reserve
  usage. Pins that haven't been checked yet count as size-0 and
  go to the bottom.

## How this differs from `:pins-check`

[`:pins-check`](../commands/pins-check.md) walks **every** pin
sequentially and writes the full output to a temp file. It's
the right tool when you want a one-shot integrity report you
can email or attach to a support thread. Useful but slow on
nodes with hundreds of pins.

S11 trades that bulk-walk for **interactivity**: pick the pin
you care about, get its integrity in one call, see the result
inline. The two commands are complementary — `:pins-check` for
the audit, S11 for the fix-loop.

## What's intentionally out of scope (v1)

- **No pin/unpin actions.** Pinning is a write op; the cockpit
  stays read-mostly. Add pins from `swarm-cli pin add` and
  they'll appear here on the next `/pins` poll (≤ 30 s).
- **No automatic integrity polling.** `/pins/check` walks the
  chunk graph — too expensive to run on a clock. Operators
  trigger it on demand.
- **No diff against a previous check.** A pin that goes from
  healthy to degraded shows up the moment you re-check it; the
  cockpit doesn't keep a history. Use `:pins-check` for a
  point-in-time snapshot if you need to compare runs.
