# S13 — Feed Timeline

> Earlier docs (and the file name `s14-feed-timeline.md`) called
> this S14. The screen is now the **13th** tab — the file name
> is kept for stable links.

A scrollable history walk of a Swarm feed. Where v1.5's
[`:feed-probe`](../commands/feed-probe.md) returns the latest
update only, S14 walks backward from the latest index and shows
each historical entry side-by-side: index, age, payload size,
and (when reference-shaped) the embedded Swarm reference.

## How to load

The screen has no auto-poll — it loads exactly when an operator
issues the verb:

```text
:feed-timeline <owner> <topic> [N]
```

`<owner>` and `<topic>` accept the same forms as `:feed-probe`:
20-byte hex address (`0x`-prefixed or bare) and either 64-hex
literal or arbitrary string (keccak256-hashed via
`Topic::from_string`).

`[N]` is optional — defaults to **50**, hard-capped at **1000**.
For larger walks, drive `:feed-probe` from a shell loop instead;
the cockpit's in-memory tar / mpsc plumbing isn't sized for
multi-thousand-entry walks.

The first lookup hits Bee's `/feeds/{owner}/{topic}` to find the
latest index — this can take 30-60 s on a fresh feed. The screen
shows a spinner until that completes; the historical chunks then
fetch in parallel (8-way bounded concurrency) so a 50-entry walk
finishes in seconds once the latest-index probe returns.

## Layout

```
┌ FEED TIMELINE  owner=0x12345678…  topic=ab12cd34…  latest=idx42  · 50 entries ─┐
│                                                                                │
│  INDEX     AGE      SIZE   TYPE      REF / ERROR                               │
│      42        3m     40   ref       e7f3a201cd…                               │
│      41       12m     40   ref       9b1c8a72f4…                               │
│      40       18m     20   raw       payload 12B                               │
│      39       28m      0   miss      [lost: 404 Not Found]                     │
│      38       45m     40   ref       12abcdef34…                               │
│      …                                                                         │
│  selected: ref=e7f3a201cd1f0e9b…                                               │
│  ↑↓/jk select   Tab switch screen   : command   q quit                         │
└────────────────────────────────────────────────────────────────────────────────┘
```

The cursor row is reverse-styled. **Miss** rows (chunk fetch
failed or didn't unmarshal as a SOC) render dim, so gaps in the
history are visible at a glance.

The selected-line detail at the bottom shows the full reference
of the cursored row when present, or the raw payload size + Unix
timestamp when the entry isn't reference-shaped.

## Keymap

| Key | Action |
|---|---|
| `↑` / `k` | Move cursor up |
| `↓` / `j` | Move cursor down |
| `PgUp` / `PgDn` | Jump 10 rows |
| `Tab` | Cycle to the next screen |
| `:` | Open the command bar (e.g. for `:inspect <ref>` on the cursored entry) |

## CI mode (`--once feed-timeline`)

```sh
bee-tui --once --json feed-timeline 0x1234… my-app/notifications 100
```

Emits structured JSON with `owner`, `topic`, `latest_index`,
`index_next`, `reached_requested`, and an `entries` array of
`{ index, timestamp_unix, payload_bytes, reference, error }`.
A snapshot-publish workflow can fetch 100 historical entries and
gate on `entries[0].index` strictly advancing across runs, or on
the `error` count not crossing a threshold.

## What it doesn't do

- **No epoch-feed walk.** v1.6 walks sequential feeds (indexes
  `0, 1, 2, …`). Epoch feeds (Swarm's older lookup scheme) are
  not yet supported in the walker.
- **No live refresh.** The walk is one-shot per verb invocation;
  there's no auto-poll. Re-run the verb to get a fresh snapshot.
- **No payload preview.** Raw-feed entries surface their byte
  size only; if you want the contents, pass the entry's index
  back through `:feed-probe` or feed it to `:inspect` when
  reference-shaped.
- **No write side.** `:feed-timeline` is read-only; updating a
  feed requires a private key + a stamp, both outside the
  cockpit's current write surface.
