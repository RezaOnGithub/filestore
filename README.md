# Filestore prototype

An end-to-end prototype of the design in [doc/DESIGN.md](doc/DESIGN.md):
a synchronous Rust scanner and catalog, an Axum server that serializes all
catalog work onto one worker thread, and a mouse-driven web UI.

Filestore catalogs files in an ordinary directory tree that you keep editing
with other tools. It records what each scan observed, recognizes exact content
(Blobs) by full reads, compares cheaply by sampling, keeps every observation
as history, and lets you record meaning (tags, Lineages of revisions) on the
content. The interface makes the strength of each piece of evidence visible:
*verified*, *plausible*, *modified?*, *changed*, *unidentified*, *gone*.

## Running it on your own data

Requirements: Linux 5.6+ (for `openat2`), Rust 1.88+, Node 20+.

```sh
cd web && npm install && npm run build && cd ..
cargo build --release
target/release/filestore serve --catalog ~/filestore-catalog --root ~/Documents
# open http://127.0.0.1:7878/
```

* `--root` is only needed the first time; it creates the catalog. The catalog
  directory must be **outside** the root, and vice versa.
* The scanner only reads: it never modifies, moves, or deletes your files.
  The catalog is as sensitive as the files, though: it holds paths, sizes,
  content digests, and tags.
* For a first try, point `--root` at a directory with a few thousand files.
  Verify reads every byte (the 4.4 GiB test corpus takes about 10 s on an
  SSD once cached; your disk sets the pace). Inventory is cheap.
* Stop the server with Ctrl-C. A running scan is cancelled and its completed
  work is published first. If the process dies instead, the next start
  records that scan as *abandoned*, and none of its evidence becomes current.

The test corpus from `.local` works too:

```sh
target/release/filestore serve --catalog .local/catalog --root .local/trent-nelson-pdfs
```

### Command line

The same binary runs scans without the UI. It takes the same exclusive
catalog lock, so it cannot run while the server holds that catalog.

```sh
filestore init    --catalog DIR --root DIR
filestore scan    --catalog DIR inventory|verify|check [PATH ...] [--depth N]
filestore suggest --catalog DIR      # print the advisors' suggestions
filestore rebuild --catalog DIR      # recompute the projection, compare with the incremental one
```

For UI development, run `npm run dev` in `web/`. It proxies `/api` to a
server on the default port.

## A tour

1. **Suggestions → Accept: inventory.** Inventory lists every directory and
   records names, inodes, sizes, and timestamps. It reads no content, so
   every file is *unidentified*.
2. **Browse → Verify.** Every file is fully read and hashed. Each distinct
   content becomes a Blob with a random public ID (`blob_…`), and identical
   files share one Blob. While a scan runs it owns the catalog: the busy bar
   shows progress, and other views wait, keeping their last data on screen.
3. **Change things outside Filestore**, then **Inventory** again:
   * Touch, chmod, or edit a file in place: it becomes **modified?**, because
     metadata differs from the read that established its content. Metadata
     alone is not proof.
   * Append to a file: its size differs, and the advisors suggest Verify.
   * Rename or move a file or directory: the old path is **gone** (see the
     *history* checkbox) and the new path is *unidentified*. A *possible
     moves* suggestion cites the shared inode; nothing transfers until you
     verify.
   * Replace a file atomically, as editors do: a new occurrence begins (new
     inode), and the occurrence history in the detail pane shows both.
   * Delete a file: it is gone from the current view. Its Blob stays in
     **Blobs → no current location**.
4. **Check.** Files up to 1 MiB are fully re-read. Larger files are sampled
   and compared against their last verified content: a match makes them
   **plausible**, a difference makes them **changed**. An edit outside the
   sampled blocks goes unnoticed; that blind spot is by design.
5. **Verify a changed file.** A *semantic proposal* appears: record the old
   and new content as revisions of one Lineage. **Review…** lets you edit the
   title and labels before accepting. The *name-sequence* advisor proposes
   Lineages for files whose names differ only in numbers or dates (try
   *Agner Fog* or *AMD64 … Volume 3* in the test corpus).
6. **Tag.** Select verified files (shift-click selects a range), then
   **Tag…**. Tags attach to content, so every copy shares them. Each
   hierarchy holds at most one tag per target. Unverified files cannot be
   tagged.
7. **Restore.** Replace the root directory with a copy (for example
   `mv root root.old && cp -a root.old root`). The next scan fails with
   *root replaced* rather than reporting everything deleted. **Review
   adoption…** starts a new identity epoch. **Check** then recovers content
   identity by path (verified for small files, plausible for large ones), and
   Verify recovers Blob IDs with their tags and revisions.

Every scan, attempt, and decision is retained. **Scans** shows each scan's
targets, results, gaps, diagnostics, and the accepted proposal it came from.
**History** lists every semantic change with before and after values.

## What is implemented

| Design area | Status |
| --- | --- |
| Inventory / Verify / Check over files, directories (with depth), or the root (§5) | ✔ |
| `openat2`-constrained resolution, no symlink following, no mount crossing (§3.4) | ✔ |
| Fresh *opened* observation per open; trigger kept separately (§3.5) | ✔ |
| Handle stability checks, one retry, independent content and location outcomes (§3.4) | ✔ |
| Blob admission by BLAKE3 digest, random public IDs, digests kept private (§4) | ✔ |
| Sample baselines and Check with full-read fallback (§6) | ✔, simplified policy (below) |
| Separate attempt/result rows, closure and immutability enforced by triggers (§9.4) | ✔ |
| Atomic publication; crash recovery as *abandoned* (§9.2) | ✔ |
| Occurrence projection: continuity by inode within an epoch, listing coverage and staleness, contradictions, last-known evidence (§7) | ✔, rebuildable (`filestore rebuild`) |
| Identity epochs from the filesystem context; root replacement detection (§3.3) | ✔, adoption without a comparison report |
| Advisors with evidence, uncertainty, provenance on acceptance, stale-proposal rejection (§10) | ✔ |
| Idempotent command acceptance (§10.1) | ✔, client request IDs only |
| Exclusive tag hierarchies on Blobs, Lineages, and revisions; Lineages with revision DAGs; operation log (§11) | ✔ |
| Extractors (§4.2) | one streaming extractor (`sniff`, format detection) |
| Verification horizon (§6.4) | setting plus *overdue* status and advisor; no score |
| Manifests, link statements, bundles (§12) | ✘ |
| Candidate bindings and the restore comparison report (§3.3) | ✘ |

## Where the prototype departs from the design

* **Sampling (§6.3).** Instead of a logarithmic byte budget with Floyd
  selection from a BLAKE3 stream, Check samples a fixed 16 blocks of 64 KiB,
  evenly spaced and always including the first and last block. Files of at
  most 16 blocks (1 MiB) are fully read instead. That caps a sampled Check at
  1 MiB and 16 seeks per file, independent of size; the policy is versioned
  and encoded in each baseline. See `crates/filestore/src/sample.rs`.
* **Schema (§13).** Flatter than the provisional schema. A scan row is both
  request and session (one session per request, Q15). Root bindings and
  activations are reduced to identity epochs, and adoption appends a new
  epoch. Extractor runs are folded into `derived_metadata`. Assignments live
  in one `tag_assignments` table. Semantic changes are stored as JSON on
  `semantic_ops`. Occurrences are a materialized projection table, not a
  cache keyed by query. Public IDs are 128 random bits.
* **Process model (Q16).** One process holds an exclusive `flock` on the
  catalog. Inside it, one worker thread owns the SQLite connection and runs
  every query, semantic operation, and scan in FIFO order. That is how a
  scan gets exclusive access. Progress and cancellation flags are shared
  memory and never touch the database.
* **Command acceptance (§10.1).** The client sends a unique request ID with
  each mutation. A retry with the same ID and payload returns the recorded
  result, and reusing an ID for different content is rejected. There is no
  server-issued witness.
* **Location checks.** The whole path is re-resolved from the root handle with
  `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_XDEV`, and the device
  and inode of the final object are compared. Ancestors are not checked one
  by one, and inode generation numbers are not recorded.
* **Listing freshness (§7.2).** A directory's listing becomes *stale* when a
  later observation of that directory shows a different mtime or ctime from
  its last complete listing.
* **User cancellation** publishes the completed part of a scan as
  `finished_with_gaps`, with a `cancelled` gap.
* **Restore.** There is no candidate scan or exception report: adoption is an
  explicit decision. Afterwards, Check compares each path with the last
  content verified at that path, in any epoch.
* **Suggestions** are computed on demand. Dismissal lasts for the browser
  session only.

## Layout

```
crates/filestore/          synchronous core: catalog, scanner, projection, queries, advisors, semantics
  src/schema.sql           SQLite schema with closure and immutability triggers
  tests/                   scanner/projection and advisor/semantic integration tests
crates/filestore-server/   `filestore` binary: CLI, Axum API, single worker thread
web/                       Vite + React + TypeScript UI (served from web/dist)
doc/                       design documents
```

`cargo test` runs the unit and integration tests. They build real
directory trees under `.local/test-tmp/`.
