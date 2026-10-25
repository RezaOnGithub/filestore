# Agent guidance

Filestore is a prototype that catalogs files in a directory tree it does not
own. A synchronous Rust core (`crates/filestore`) holds the catalog,
scanner, evidence interpretation, advisors, and semantic records. A server
(`crates/filestore-server`, binary `filestore`) runs every catalog job on
one worker thread behind an Axum API. The web UI lives in `web/` (Vite,
React, TypeScript).

Read before changing behavior:

* [README.md](README.md): what the prototype does and how to run it.
* [doc/DESIGN.md](doc/DESIGN.md): the model and its vocabulary (§2,
  terms), the read procedure (§3.4–3.5), Check (§6), the projection rules
  (§7.3), and publication (§9).
* [doc/IMPLEMENTATION.md](doc/IMPLEMENTATION.md): where and why the code
  departs from the design, and the edge cases it handles. Keep it current.
* [doc/PLANS.md](doc/PLANS.md): planned features and the extractor recipe.

## Commands

```sh
cargo build                      # workspace; --release for realistic timings
cargo test                       # unit + integration tests (create trees under .local/test-tmp)
cargo clippy --all-targets       # keep warning-free
(cd web && npm install && npm run typecheck && npm run build)   # UI; served from web/dist
target/release/filestore serve --catalog .local/cat --root <dir>   # http://127.0.0.1:7878/
target/release/filestore rebuild --catalog .local/cat              # projection must come out identical
python3 doc/schema_dot.py > doc/SCHEMA.dot                         # after any schema change
```

Before finishing a change: `cargo test`, `cargo clippy --all-targets` with
no warnings, and for UI changes `npm run typecheck && npm run build`.

## Invariants

These are the design's guarantees. Each is protected by tests;
`tests/scan.rs`, `tests/regressions.rs`, and `tests/limitations.rs` name the
scenarios.

1. **Evidence is append-only and closed.** Rows owned by a scan (observations,
   enumerations, reads, results, diagnostics, derived metadata) are inserted
   only while the scan runs. They are never updated or deleted, and listing
   entries cannot be added after the listing's result. Triggers in
   `schema.sql` enforce this. Never weaken them; add triggers for new
   evidence tables.
2. **Measurements belong to the object actually opened.** Every open for a
   read or listing creates a fresh `opened` observation from the handle. The
   listing entry that prompted the work is only the trigger, and no property
   flows from one to the other.
3. **Content and location outcomes are independent.** Only a full read with
   a stable handle recognizes a Blob, and that admits it even if the
   location check fails. Only full recognition plus a `confirmed` location
   makes a file *verified*. A sample or size comparison never verifies,
   never sets `verified_at`, and never erases a contradiction.
4. **Absence comes only from a complete, stable, location-confirmed
   listing.** Failures, unvisited directories, and boundaries are gaps, never
   emptiness.
5. **The projection is deterministic and rebuildable.** `project.rs` must
   produce the same `occurrences` incrementally and from
   `project::rebuild`. End new scenario tests with
   `env.assert_rebuild_matches()`. If you change a projection rule, existing
   catalogs need `filestore rebuild`.
6. **Content digests and sample tokens never leave the catalog.** They
   must not appear in API responses, logs, diagnostics, suggestion IDs, file
   names, or extractor inputs. Content is named only by random public Blob
   IDs.
7. **Exclusive, serialized access.** `crates/filestore` stays synchronous:
   no async, no threads, one connection. In the server, every database
   access goes through `Handle::call` on the worker. Only status, progress,
   and cancellation bypass it, through shared memory.
8. **Advisors read the catalog only.** They read no file content and write
   nothing (`generating_suggestions_writes_nothing`). Accepting a suggestion
   records its provenance atomically with the resulting scan request or
   semantic operation.
9. **Semantic changes are logged.** Every mutation of tags, Lineages, or
   revisions writes a `semantic_ops` row with before/after values in the
   same atomic section. Revisions' content identity is immutable; identities
   are tombstoned, never deleted.
10. **The scanner never modifies the managed tree** and never follows
    symlinks or crosses mounts. All resolution beneath the root goes through
    `fsx` (`openat2` with `RESOLVE_BENEATH | NO_SYMLINKS | NO_XDEV`); do not
    add unconstrained `open` calls on paths under the root.

## Conventions

* **Transactions.** Use `catalog::atomic(&conn)` (savepoints), not
  `Connection::transaction`. Atomic sections nest inside the scanner's
  batch and the command ledger.
* **Errors.** `Error::Invalid` → 400, `NotFound` → 404, `Conflict` → 409
  (stale suggestions, reused request IDs). Use them deliberately; the UI
  shows the message.
* **Paths.** Root-relative byte strings in the database. At the API
  boundary use `pathenc` (lossless `%XX` text form). `""` is the root.
* **Statuses.** Evidence statuses are computed once, in `query.rs`
  (`file_status`, `build`). The UI only presents them (`web/src/util.ts`
  `explain`, `STATUS_LABEL`). Add a status in both places and in
  `web/src/types.ts`.
* **Vocabulary** (DESIGN.md §2): Inventory, Verify, Check, *full read*,
  Blob, Location, Observation, Occurrence, Lineage, revision. Avoid "full
  scan" and "partial scan"; a limited scope is not a gap.
* **Schema changes.** Edit `schema.sql`, then regenerate `doc/SCHEMA.dot`
  and update `doc/schema_dot.py` (groups, notes, logical references) if you
  add tables. There are no migrations yet (`SCHEMA_VERSION = 1`): a changed
  schema needs a fresh catalog. Say so in the change description.
* **Extractors.** Follow the rules and recipe in doc/PLANS.md §3. Outputs
  are a function of the bytes and keyed by `(name, version, config)`; bump
  `version` when output can change.
* **Tests.** Use the fixture in `tests/common/mod.rs`, which creates a
  catalog and root under `.local/test-tmp`. Build real trees and run real
  scans. Prefer an integration test over mocking the filesystem. A bug in
  evidence handling gets a regression test in `tests/regressions.rs`.
* **UI.** Mouse-driven and utilitarian; no component library. Fetch with
  `useApi` (it defers background refreshes while a scan holds the catalog).
  Send a `request_id` with mutations that go through the ledger (`api.ts`
  does).

## Workspace hygiene

* Put scratch files, logs, throwaway scripts, demo catalogs, and test
  trees in `.local/`, which is gitignored. Do not use `/tmp`.
* `.local/trent-nelson-pdfs` is the user's test corpus. Never modify it.
  For experiments that change files, make a copy, for example
  `cp -a --reflink=always .local/trent-nelson-pdfs .local/demo-root`, which
  is instant on btrfs.
* A catalog is locked by the process using it. Stop a server before
  running CLI scans on its catalog. Stop background servers by PID; a
  `pkill -f` pattern can match its own shell command line.
* Commit only when asked. End commit messages with the co-author trailer
  the user requests.
