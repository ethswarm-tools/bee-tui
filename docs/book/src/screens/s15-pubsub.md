# S15 — Pubsub watch

Live tail of PSS topic subscriptions and GSOC `(owner, identifier)`
subscriptions, merged into a single chronological timeline.
The receiver-side complement to v1.3's
[`:gsoc-mine`](../commands/bar.md) and `:pss-target` writer verbs:
operators can finally see the messages those senders produce
without leaving the cockpit.

## How to start a subscription

The screen has no auto-load. Subscriptions are started by verb:

```text
:pubsub-pss   <topic>
:pubsub-gsoc  <owner> <identifier>
```

`<topic>` accepts the same forms as `:feed-probe`:

- **64 hex chars** (with or without `0x`) is the raw 32-byte topic.
- **Anything else** is `keccak256(utf8(s))`, mirroring bee-js's
  `Topic.fromString`.

`<owner>` is a 20-byte Ethereum address (`0x`-prefixed or bare).
`<identifier>` is a 32-byte SOC identifier (64 hex chars,
`0x`-prefixed or bare).

Each subscription opens a WebSocket against Bee's
`/pss/subscribe/{topic}` or `/gsoc/subscribe/{soc-address}` and
forwards every delivered frame into the screen's ring buffer.
The verb switches to S15 immediately so the operator sees the
"0 messages" state until the first frame arrives.

Re-issuing for an already-watched `(topic)` or `(owner, identifier)`
errors with a clear message — no silent duplicate sockets.

## Layout

```
┌ PUBSUB WATCH  · 2 active subs · 17 messages ─────────────────────────────┐
│                                                                           │
│  TIME      KIND   CHANNEL       SIZE   PREVIEW                            │
│  10:14:32  PSS    abc1234567…    18    hello cockpit!                     │
│  10:14:31  GSOC   ee7f3a2018…    32    deadbeef…                          │
│  10:14:30  PSS    abc1234567…    42    {"event":"ping","seq":12}          │
│  ...                                                                      │
│                                                                           │
│  channel: 0xabc1234567890abcdef…fedcba0987654321 · 18 bytes               │
│  data: hello cockpit!                                                     │
│                                                                           │
│  ↑↓/jk select   c clear timeline   Tab switch screen   : command   q quit │
└───────────────────────────────────────────────────────────────────────────┘
```

The cursor row is reverse-styled. **GSOC** rows tint blue so PSS
and GSOC are distinguishable at a glance even after the kind
column scrolls offscreen.

The two-line detail strip shows the full channel hex and the
smart-preview of the cursored row's payload (capped at 200 chars).
"Smart" means: ASCII when ≥ 75 % of bytes are printable, hex
otherwise. Empty payloads render as `(empty)`.

## Keymap

| Key | Action |
|---|---|
| `↑` / `k` | Move cursor up |
| `↓` / `j` | Move cursor down |
| `PgUp` / `PgDn` | Jump 10 rows |
| `c` | Clear the timeline (subscriptions stay open) |
| `Tab` | Cycle to the next screen |
| `:` | Open the command bar |

## Stopping subscriptions

```text
:pubsub-stop                        # cancels every active subscription
:pubsub-stop pss:abc1234567…        # cancels just the matching one
:pubsub-stop gsoc:0xabc…:def0…      # GSOC subs are keyed by owner:id
```

Sub-IDs are reported by the `:pubsub-pss` / `:pubsub-gsoc`
"subscribed: …" line. The cockpit's root cancellation token also
fires on quit, so operators don't need to remember to issue
`:pubsub-stop` before exiting.

## Filtering the timeline

```text
:pubsub-filter <substring>          # show only matching rows
:pubsub-filter-clear                # remove the active filter
```

Case-insensitive substring match against the channel hex OR the
smart-preview of the payload. The underlying ring still receives
every message — filtering is presentation-only, so clearing the
filter restores the full view without re-subscribing.

## Persisting + replaying history (v1.8 / v1.9)

Set `[pubsub].history_file` in `config.toml` to write every
delivered frame to a JSONL file:

```toml
[pubsub]
history_file = "/var/lib/bee-tui/pubsub.jsonl"
rotate_size_mb = 64        # roll over at 64 MiB (default; 0 disables)
keep_files     = 5         # retain .1 .. .5 (default)
```

Files are created with mode `0600` (owner-only). When the active
file crosses `rotate_size_mb`, it's renamed to `<path>.1` (older
rotations shift to `.2` .. `.N`; oldest beyond `keep_files` is
unlinked) and a fresh empty file takes its place.

To browse a past session without re-subscribing:

```text
:pubsub-replay <path>
```

Loads the file back into the S15 ring (oldest → newest, capped at
500 entries). Bad lines are skipped with a warn log; replay does
not start any watchers.

## What it doesn't do

- **No live "tail since T-30s".** WebSocket subscriptions only
  deliver messages sent *after* the subscription opens — start the
  sub before the publisher does. (Past sessions can be loaded via
  `:pubsub-replay`; live ones cannot be rewound.)
- **No write side.** Sending PSS / GSOC requires a stamp + private
  key, both outside the cockpit's current write surface. Use
  bee-cli or a dApp for that.
- **No `--once` mode.** A live tail doesn't fit one-shot exit
  semantics; if you want to gate on "did this topic see N
  messages in T seconds", script it with a separate tool.
