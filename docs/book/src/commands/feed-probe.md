# `:feed-probe`

Single-shot lookup of the latest update of a Swarm feed. Read-only
(no chain interaction, no stamp consumption); the natural
counterpart to bee-tui's existing `:gsoc-mine` and `:pss-target`
verbs which serve the writer side.

```text
:feed-probe <owner> <topic>
```

`<owner>` is a 20-byte Ethereum address — `0x`-prefixed or bare
40-hex (case-insensitive).

`<topic>` accepts two forms, picked by heuristic:

- **64 hex chars** (with or without `0x`) is treated as the raw
  32-byte topic.
- **Anything else** is `keccak256(utf8(s))`, mirroring bee-js's
  `Topic.fromString` and bee-cli's `topic-from-string`. Operators
  rarely think in raw 32-byte topics; they think in
  `"my-app/notifications"`.

## Output

The verb returns an "in flight" notice; Bee's `/feeds/{owner}/{topic}`
lookup can take 30-60 s on a fresh feed (epoch index walk), so the
result lands asynchronously on the command bar.

```text
:feed-probe 0x1234… my-app/notifications
→ feed-probe owner=12345678 in flight — result will replace this line (first lookup can take 30-60s)

(several seconds later …)
→ feed-probe owner=12345678 · index=42 · ts=1762000000 (3m) · ref=e7f3a201… (4123ms)
```

For raw feeds whose payload isn't a 32 / 64-byte reference, the tail
shows `payload=<n>B` instead of `ref=...`.

## CI mode (`--once feed-probe`)

```sh
bee-tui --once --json feed-probe 0x1234… my-app/notifications
```

Emits structured JSON with `owner`, `topic`, `topic_was_string`,
`topic_string`, `index`, `index_next`, `timestamp_unix`,
`payload_bytes`, and `reference`. A snapshot-publish workflow can
poll a known feed and gate on `index` advancing across runs:

```sh
PREV=$(cat /tmp/last-feed-index)
NEXT=$(bee-tui --once --json feed-probe $OWNER $TOPIC | jq -r .data.index)
if [[ "$NEXT" == "$PREV" ]]; then
  echo "feed didn't advance — alert"
  exit 1
fi
```

## When to use it

- Confirming a writer-side workflow actually published an update
  (smoke test after `:upload-file` + a separate `update_feed`
  call).
- CI gates that should fail when an upstream feed stops advancing
  (broken publisher, out-of-funds signer, etc.).
- Investigating "is this feed alive?" without firing up a full
  bee-cli or bee-js setup.

## What it doesn't do

- **No history walk** — only the latest update is fetched. A
  Feed Timeline screen with epoch history is on the v1.6 roadmap.
- **No payload decoding** — when `reference_hex` is `None` the
  verb just reports the byte size; if you want the contents,
  pass it through `:inspect` or `download_data` separately.
- **No write side.** `:feed-probe` is read-only; updating a feed
  requires a private key + a stamp, both outside the cockpit's
  current write surface.
