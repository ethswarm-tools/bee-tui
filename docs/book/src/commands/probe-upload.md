# `:probe-upload`

Uploads one synthetic 4 KiB chunk to Bee and reports the
end-to-end latency. The cockpit is otherwise read-only —
this is the deliberate exception.

```text
:probe-upload <batch-prefix>
```

`<batch-prefix>` is the 8-character hex shown in the S2 table
(trailing `…` allowed; bee-tui strips it). The chosen batch
must be `usable` and have `batch_ttl > 0`.

## What it answers

> *"Can my node actually take a stamp + persist a chunk + return
>  its reference, end-to-end?"*

`/readiness` returning 200 means Bee's HTTP server is up. It
does **not** mean the storage path works — a corrupted RocksDB,
an exhausted disk, or a misconfigured stamp signer can all
return a healthy `/readiness` while uploads fail. `:probe-upload`
exercises the same path real uploads take.

## Output

The verb returns immediately with an "in flight" notice; the
actual outcome lands on the command bar when Bee responds.

```text
:probe-upload a1b2c3d4
→ probe-upload to batch a1b2c3d4… in flight — result will replace this line

(a few hundred ms later …)
→ probe-upload OK in 245ms — batch a1b2c3d4…, ref e7f3a201…
```

On failure:

```text
→ probe-upload FAILED after 312ms — batch a1b2c3d4…: 422 Unprocessable Entity
```

## Cost

One stamped chunk on the chosen batch:

- **Bucket cost** — 1 collisions count on whichever bucket the
  chunk address falls in. With a healthy batch (depth ≥ 22,
  utilization « bucket_capacity) this is invisible.
- **PLUR cost** — `current_price` PLUR per chunk per block,
  times the batch's remaining TTL in blocks. With typical
  amounts that's on the order of `1e-12` BZZ per probe — well
  under a millionth of a cent.

Each invocation generates a unique chunk (timestamp-randomised
payload) so Bee's content-addressing dedup doesn't short-circuit
the second probe and skew the latency reading.

## When to use it

- After a Bee restart, before resuming production uploads.
- Diagnosing intermittent upload failures: run a few back-to-back,
  watch the latency distribution.
- Verifying a stamp is actually usable end-to-end (the bucket the
  chunk lands in might already be saturated even when
  `worst_bucket_pct` looks fine — `:probe-upload` will tell you).

## What it doesn't do

- **Doesn't verify retrieval.** A future iteration may follow up
  with a `GET /chunks/<ref>` to measure full round-trip; for now
  the verb stops at upload success.
- **Doesn't run repeatedly.** One call = one chunk. No built-in
  loop. If you need throughput / latency curves, drive it from
  `bee-bench` instead.
- **Doesn't pick a batch for you.** Explicit `<batch-prefix>` is
  required so you always know which batch you stamped against.
