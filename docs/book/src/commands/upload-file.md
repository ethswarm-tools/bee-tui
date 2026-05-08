# `:upload-file`

Uploads a single local file via `POST /bzz` to a chosen postage
batch and returns the resulting Swarm reference. Unlike
[`:probe-upload`](./probe-upload.md), which posts a synthetic
4 KiB chunk to verify the upload path, `:upload-file` ships an
actual operator-supplied file the same way `swarm-cli upload`
would.

```text
:upload-file <path> <batch-prefix>
```

`<path>` is a local file (directories are rejected; a future
release will add a `:upload-collection` verb for that). The file
is capped at **256 MiB** so the cockpit's event loop doesn't
stall while reading it; for larger uploads use `swarm-cli` where
the upload runs out of process.

`<batch-prefix>` is the 8-character hex shown in the S2 table
(trailing `…` allowed; bee-tui strips it). The chosen batch must
be `usable` and have `batch_ttl > 0`.

## Content type

Inferred from the extension for common types
(`.html` → `text/html`, `.json`, `.png`, `.pdf`, `.tar.gz`,
`.wasm`, …). Anything not in the table is uploaded as
`application/octet-stream` — Bee will still serve it on download
but the `Content-Type` header on `GET /bzz/<ref>` will be the
generic value. Override semantics are not exposed yet (no
`--content-type` flag); if you need a specific MIME, rename the
file or use `swarm-cli`.

## Output

The verb returns immediately with an "in flight" notice; the
actual outcome lands on the command bar when Bee responds.

```text
:upload-file ./build/index.html a1b2c3d4
→ upload-file (12_345B) to batch a1b2c3d4… in flight — result will replace this line

(a few hundred ms later …)
→ upload-file OK in 312ms — 12345B → ref e7f3a201… (batch a1b2c3d4…)
```

On failure:

```text
→ upload-file FAILED after 412ms — batch a1b2c3d4…: 413 Payload Too Large
```

## CI mode (`--once upload-file`)

The same verb is available out of the TUI for snapshot-publish
workflows:

```sh
bee-tui --once --json upload-file ./dist/site.html a1b2c3d4
```

Emits structured JSON including `reference`, `size`,
`content_type`, and `batch_id` so a downstream step can pin the
ref or post it to a release artefact.

## When to use it

- Publishing a single file (a static page, a release asset, a
  PDF) without leaving the cockpit.
- Pinning a known input with a known batch + known content type
  so the swarm hash is reproducible across runs.
- Verifying a fresh batch is wired correctly by uploading a real
  file end-to-end (`:probe-upload` covers the chunk path; this
  covers the manifest path).

## What it doesn't do

- **No directory upload.** Single-file scope only — collection
  upload comes later.
- **No retrieval check.** Stops at upload success; pair with
  `:inspect <ref>` after if you want to verify the manifest is
  parseable.
- **No automatic stamp picking.** Explicit `<batch-prefix>` is
  required so you always know which batch your upload was
  stamped against.
