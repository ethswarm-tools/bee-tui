# `:upload-collection`

Recursively walks a local directory and uploads it as a Swarm
collection via `POST /bzz` (tar). The natural complement to
[`:upload-file`](./upload-file.md) for publishing static sites,
build-output bundles, and dApp distributions without leaving the
cockpit.

```text
:upload-collection <dir> <batch-prefix>
```

`<dir>` is a local directory. The walker skips:

- **Hidden entries** — anything whose name starts with `.`
  (`.git`, `.env`, `.DS_Store`, etc.).
- **Symlinks** — never followed, regardless of target. Defends
  against accidentally publishing files outside the collection
  root.
- **Non-UTF-8 names** — Bee's manifest forks are UTF-8 only;
  silently dropped.

Caps mirror `:upload-file`'s ceilings:

- **256 MiB total** across all entries.
- **10 000 entries** maximum.

Path normalisation: every entry's tar path is the relative path
from `<dir>`, with forward slashes regardless of host OS. So
`./dist/assets/logo.png` becomes `assets/logo.png` in the
manifest.

## Default index

When the walked tree contains an `index.html` at the **root**
(depth 1), it's auto-set as the collection's
`Swarm-Index-Document` header — Bee then serves that file when
a client requests `GET /bzz/<ref>/`. Nested `index.html` files
inside subdirectories are uploaded as ordinary entries; no
implicit index promotion.

## Output

Returns immediately with an "in flight" notice including the
entry count, total byte size, and default-index path; the
actual outcome lands when Bee responds.

```text
:upload-collection ./dist a1b2c3d4
→ upload-collection 47 files (3_241_092B) · default index=index.html to batch a1b2c3d4… in flight — result will replace this line

(several hundred ms later …)
→ upload-collection OK in 412ms — 47 files, 3241092B → ref e7f3a201… (batch a1b2c3d4…) · index=index.html
```

On failure:

```text
→ upload-collection FAILED after 1240ms — ./dist → batch a1b2c3d4…: 413 Payload Too Large
```

## CI mode (`--once upload-collection`)

```sh
bee-tui --once --json upload-collection ./dist a1b2c3d4
```

Emits structured JSON with `reference`, `entry_count`,
`total_bytes`, `default_index`, and `batch_id` so a snapshot-
publish workflow can pin the ref or post the URL without
parsing the human line.

## When to use it

- Publishing a static-site / dApp distribution (the canonical
  `dist/` directory) end-to-end from the cockpit.
- Pinning a reproducible swarm hash for a directory tree —
  walking is deterministic (sorted entries, no time-of-day
  inputs) so two runs over identical content produce identical
  references.
- Verifying a fresh batch is wired correctly by uploading a
  small directory end-to-end (the manifest path, with forks).

## What it doesn't do

- **No recursive symlink follow.** If you need symlink targets
  uploaded, materialise them locally (`cp -L`) first.
- **No explicit index override.** v1.5 ships the
  auto-detect-`index.html` path only. A future iteration may add
  `--index <path>` for cases where the entry file is named
  differently.
- **No retrieval check.** Stops at upload success; pair with
  `:inspect <ref>` after if you want to verify the manifest is
  parseable.
- **No automatic stamp picking.** Explicit `<batch-prefix>` is
  required so you always know which batch the upload was
  stamped against.
