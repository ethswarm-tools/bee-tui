# Stamp dry-run previews

Four read-only command-bar verbs that answer *"what would happen
if I…"* questions about postage batches without issuing any
chain-bound write. Useful when you want to plan a topup, dilute, or
fresh buy and need the BZZ cost / TTL impact ahead of time.

| Verb | Args | Answers |
|---|---|---|
| `:topup-preview` | `<batch-prefix> <amount-plur-per-chunk>` | new TTL + BZZ cost of adding this much per-chunk PLUR |
| `:dilute-preview` | `<batch-prefix> <new-depth>` | new capacity, halved TTL, depth delta (cost is always 0 BZZ — dilute redistributes the existing balance) |
| `:extend-preview` | `<batch-prefix> <duration>` | per-chunk PLUR + BZZ cost to gain that much TTL |
| `:buy-preview` | `<depth> <amount-plur-per-chunk>` | TTL, capacity, and BZZ cost of a hypothetical fresh batch |
| `:buy-suggest` | `<size> <duration>` | minimum `(depth, amount)` that covers the target — the inverse of `:buy-preview` |

`<batch-prefix>` is the 8-character hex shown in the S2 table
(trailing `…` is allowed; bee-tui strips it). Ambiguous prefixes
print the matches and ask for a longer prefix.

`<duration>` accepts `30d`, `12h`, `90m`, `45s`, or plain seconds.

`<size>` (for `:buy-suggest`) accepts `5GiB`, `2TiB`, `512MiB`,
`100MB`, `4096B`, or just plain bytes. Single-letter shorthands
(`5G`, `2T`, `100M`, `4K`) default to **binary** (powers of two)
because Bee batch capacities are always `2^depth × 4 KiB`. Decimal
suffixes (`GB`, `MB`) get the SI 1000-based interpretation if you
explicitly use them.

`<amount-plur-per-chunk>` is the per-chunk PLUR amount — the same
field stored on the batch. 1 BZZ = 10¹⁶ PLUR; for reference the
default mainnet buy uses ~414 720 000 000 000 000 PLUR (≈ 0.04 BZZ
on a depth-20 batch).

## Worked examples

```text
:topup-preview a1b2c3d4 100000000000
→ topup-preview a1b2c3d4…: +0.0419 BZZ (delta 100000000000 PLUR/chunk),
  TTL 47d 12h → 70d  6h
```

```text
:dilute-preview a1b2c3d4 23
→ dilute-preview a1b2c3d4…: depth 22→23, capacity 16.0 GiB→32.0 GiB,
  TTL 47d 12h→23d 18h, cost 0 BZZ
```

```text
:extend-preview a1b2c3d4 30d
→ extend-preview a1b2c3d4… +30d  0h: cost 0.0078 BZZ
  (1860000000000 PLUR/chunk), TTL 47d 12h → 77d 12h
```

```text
:buy-preview 22 100000000000000
→ buy-preview depth=22 amount=100000000000000 PLUR/chunk:
  capacity 16.0 GiB, TTL 47d 12h, cost 41.9430 BZZ
```

```text
:buy-suggest 5GiB 30d
→ buy-suggest 5.0 GiB / 30d  0h: depth=21 amount=518400000000 PLUR/chunk
  → capacity 8.0 GiB, TTL 30d  0h, cost 21.7268 BZZ
```

`:buy-suggest` is the inverse of `:buy-preview`. Operators usually
think *"I want 5 GiB for 30d"* — not *"depth=21, amount=5.18e11"*.
The suggester rounds depth up to the next power of two (so the
actual capacity is always ≥ your target, with the headroom shown
verbatim) and rounds duration up in chain blocks (so actual TTL ≥
your target). Pass the suggested numbers to the real `bee postage
buy` / `swarm-cli stamp buy` if you want to execute.

## Why dry-run, not buy?

bee-tui is **read-only by design** (PLAN principle 3). Previews
let operators get the predictive answers they normally have to
leave the cockpit for (`swarm-cli stamp buy --dry-run`,
`calculate_bzz.sh`) without bee-tui ever issuing a write. If you
want to actually execute the buy, copy the numbers into
`swarm-cli` or your scripted flow.

## Formulas

Every formula matches the canonical math used across `swarm-cli`,
`beekeeper-stamper`, `gateway-proxy`, and `bee-scripts`:

```text
cost_bzz   = amount × 2^depth / 1e16
ttl_blocks = amount / current_price
ttl_secs   = ttl_blocks × 5  (Gnosis blocktime)
capacity   = 2^depth × 4 KiB

dilute(d → d+k):
  new_amount = old_amount / 2^k
  new_ttl    = old_ttl / 2^k
  new_cap    = capacity × 2^k
  cost       = 0

buy-suggest (target_bytes, target_secs):
  chunks_needed = ceil(target_bytes / 4096)
  depth         = max(17, ceil(log2(chunks_needed)))   # round up; clamp to Bee minimum
  amount        = ceil(target_secs / 5) × current_price # round up in blocks
```

`current_price` comes from S1's `/chain-state` poll — if the
header still says "loading…" the preview will tell you the chain
price isn't ready yet and to retry.
