# Filestore prototype: plans and recipes

This document has three parts:

1. Plans for **tag browsing and search**.
2. Plans for **manifests, link statements, and bundles** (DESIGN.md §12).
3. A **recipe for adding extractors**, both the streaming kind supported
   today and the seeking kind needed for thumbnailers.

They build on the prototype described in [IMPLEMENTATION.md](IMPLEMENTATION.md).
File references are relative to `crates/filestore/src/` unless stated
otherwise.

Several pieces of groundwork are shared between the plans:

| Groundwork | Needed by |
| --- | --- |
| Request-level extractor selection | manifests, expensive extractors (thumbnails), backfill |
| A comparison Blob per file target in Check/Verify | bundles, "check this candidate after a move", restore report |
| An expected-tree comparison report | bundles, restore adoption (§3.3) |
| A rebuildable tag closure | tag search, tag counts |

---

## 1. Tag browsing and search

### Current state

* **Tags tab.** Lists hierarchies as trees with usage counts. Selecting a
  tag lists the Blobs (with their current paths), Lineages, and revisions
  carrying it or any descendant tag (`semantic::tag_targets`).
* **Browse.** A marker shows tagged content; the detail pane lists tags and
  allows removal.
* **Blobs tab.** Filters for *tagged* and *in a Lineage*, plus a path/Blob
  ID substring search.
* **`GET /api/search`.** A path substring search. The UI uses it only in the
  "Add revision" picker.

There is no way to combine conditions ("Topic/Hashing, verified, PDF, under
`papers/`"), no free-text name search in the UI, and no tag reorganization
beyond renaming.

### Goals

* **One "Find" view.** Combines tag facets, evidence status, format, path,
  and name text, and returns files (current occurrences) as well as Lineages
  and revisions.
* **Respect the evidence model.** Matches through Blob tags are exact only
  for *verified* files. Plausible or modified files show dimmed tags, as in
  Browse, and can be excluded.
* **Respect assignment scopes (§11).** Queries say whether they match Blob,
  Lineage, or revision assignments, or any of them. There is still no
  synthesized "effective tag".
* **Fast at 100k–1M locations.**

### Backend

1. **Tag closure.** Add a rebuildable `tag_closure(ancestor_id,
   descendant_id, depth)` table. Semantic operations maintain it in the same
   transaction as tag creation (and later reparenting), and
   `project::rebuild` can recompute it. "Tag X including descendants" then
   becomes a join instead of the in-memory walk in `TagIndex::subtree`.
2. **Name index.** Add an FTS5 table
   `location_text(location_id, name, path)` with `tokenize='trigram'`, which
   gives fast case-insensitive substring search. The scanner inserts a row
   whenever it interns a new location (`Run::child_location`). Names that are
   not UTF-8 are indexed lossily; this affects search only, since storage
   stays lossless. The index is rebuildable from `locations`.
3. **A structured query endpoint.** `POST /api/find` takes a JSON body
   rather than a string language, so the UI can build it from facets:

   ```json
   {
     "tags": [{"tag": "tag_…", "descendants": true, "scope": "blob"}],
     "tags_mode": "all",
     "status": ["verified", "plausible"],
     "formats": ["pdf"],
     "under": "papers",
     "text": "hash",
     "size": {"min": 0, "max": null},
     "kinds": ["file", "lineage", "revision"],
     "limit": 500
   }
   ```

   * **SQL.** The query is compiled to SQL over `occurrences` (current, in
     the active epoch), joined with `tag_assignments` through the closure,
     `derived_metadata` (for format), and the FTS table.
   * **Status.** Status filters reuse `query::file_status`, so Find and
     Browse agree.
   * **Results.** File results reuse `EntryView` with an added `path`
     column. Lineage and revision results reuse the existing summaries.
   * **Facet counts.** Counts per tag, status, and format for the current
     result set are returned so the UI can show them. They are cached per
     `(publication_seq, semantic_ops count)`.
4. **Conflict view.** For a Lineage, report hierarchies in which the
   Lineage, its revisions, and their Blobs select *different* tags. §11
   requires these to be shown as conflicting contextual assignments, not
   merged.
5. **Tag management operations.** Each is a semantic operation with
   before/after values:
   * reparent within a hierarchy (cycle check);
   * merge tag A into B (moves assignments; A is tombstoned);
   * tombstone (the name stays reserved);
   * rename a hierarchy.

   Tombstoned tags stay resolvable in history and drop out of pickers.

### UI

* **Find tab.**
  * The left panel holds facets: each hierarchy as a collapsible tree with
    counts and tri-state checkboxes (include, exclude, ignore), then status
    chips, formats, and an "under directory" picker.
  * A free-text box sits on top.
  * The results table is Browse's entry table plus a path column. It has the
    same selection bar (Inventory, Verify, Check, Tag, Add to Lineage) and
    "Open in Browse".
* **Entry points.**
  * Clicking a tag anywhere (Tags tab, tag chips, detail pane) opens Find
    with that tag selected.
  * The Tags tab gains drag-and-drop or menu actions for reparenting and
    merging.
* **Saved searches** live in `localStorage` at first and could become
  catalog objects later.

### Tests

* **Closure.** The closure equals a recursive CTE after any sequence of
  tag operations, and after rebuild.
* **Scope.** A Blob tag never matches a revision-scoped query, and vice
  versa.
* **Exactness.** Plausible occurrences match only when requested. Results
  for ended or previous-epoch occurrences appear only as history.
* **Names.** FTS results match `LIKE` results for ASCII names, including
  `%` and `_` literals.

---

## 2. Manifests, link statements, and bundles

DESIGN.md §12 defines the model, and Q13/Q14 in OPEN_QUESTIONS.md are still
under review. The plan below keeps the design's semantics and maps them
onto the prototype's flatter schema, in four phases. Each phase is usable
on its own.

### Phase 0 — groundwork

1. **Request-level extractor selection.** Add
   `extractors: Vec<ExtractorRequest>` to `ScanRequest` (Verify only), and
   an optional `manifest` on file targets:

   ```rust
   pub struct ExtractorRequest { pub name: String, pub config: String }
   pub struct ManifestRequest { pub extractor: String, pub config: String, pub schema: String /* schema public ID */ }
   ```

   * **Registry.** `extract::registry()` maps names to constructors.
     `sniff` stays in the default set.
   * **Recording.** `scans.policy` records what actually ran (name,
     version, config). That is the prototype's equivalent of
     `scan_extractions` and `extractor_runs`.
   * **Check stays extractor-free.** Check rejects extractor requests,
     per §4.2.
2. **A comparison Blob for file targets.** Add
   `Target { compare_blob: Option<String> }`.

   `Run::check_file` uses it instead of `comparison_target` and records
   `compare_source = 'requested'`. This also implements the proposal's
   individual-file override (§13.1), useful for "Check whether this new file
   is the Blob that disappeared from over there".
3. **An expected-tree report module** (`report.rs`). Input: a mapping from
   relative path to expected Blob, a base directory, and the scan that
   checked it. Output, per §3.3:
   * every expected path classified exactly once as verified match, sample
     match, changed, missing, or unresolved;
   * files under the base outside the mapping counted as *added*;
   * structural differences listed;
   * the exception budget computed.

   Missing requires the containing directory to have a complete, stable,
   confirmed listing in the checked scan. An expected path under an
   unlisted directory is unresolved, never missing. The same module powers
   both bundle checks and the restore comparison report that section 1.3 of
   IMPLEMENTATION.md lists as omitted.

### Phase 1 — JSON extractors, schemas, and manifests

* **Extractors.** Three built-in streaming extractors (see the recipe in
  §3):
  * `json`: one record;
  * `jsonl`: one record per line;
  * `json-schema`: parses and checks a schema.

  Each buffers at most a configured limit (for example 64 MiB) and fails
  with a diagnostic beyond it. Validation uses a pinned JSON Schema dialect
  (2020-12) through a Rust validator crate, with remote `$ref` rejected.
* **Tables:**

  ```sql
  CREATE TABLE manifest_schemas (
      id INTEGER PRIMARY KEY, public_id TEXT NOT NULL UNIQUE,
      name TEXT NOT NULL, version TEXT NOT NULL,
      derived_metadata_id INTEGER NOT NULL UNIQUE REFERENCES derived_metadata(id),
      semantic_op_id INTEGER NOT NULL REFERENCES semantic_ops(id),
      created_at INTEGER NOT NULL, UNIQUE (name, version)) STRICT;
  CREATE TABLE manifests (
      id INTEGER PRIMARY KEY, public_id TEXT NOT NULL UNIQUE,
      blob_id INTEGER NOT NULL REFERENCES blobs(id),
      extractor TEXT NOT NULL, version TEXT NOT NULL, config TEXT NOT NULL,
      schema_id INTEGER NOT NULL REFERENCES manifest_schemas(id),
      record_count INTEGER NOT NULL, scan_id INTEGER NOT NULL REFERENCES scans(id),
      read_id INTEGER NOT NULL REFERENCES reads(id), created_at INTEGER NOT NULL,
      UNIQUE (blob_id, extractor, version, config, schema_id)) STRICT;
  CREATE TABLE manifest_records (
      manifest_id INTEGER NOT NULL REFERENCES manifests(id),
      ordinal INTEGER NOT NULL, data BLOB NOT NULL,   -- SQLite JSONB
      PRIMARY KEY (manifest_id, ordinal)) STRICT;
  ```

  The closure triggers (insert only while the scan runs) and the immutable
  and no-delete triggers apply as they do for `derived_metadata`.
* **Admission.** In `Run::admit`, a manifest request validates *every*
  record. Validation is all or nothing: on any failure no manifest is
  stored, the Blob stays an ordinary Blob, and a diagnostic lists the
  failing ordinals. Records are inserted with the manifest in the read's
  savepoint, so they are admitted with the content result and do not depend
  on publication (§9.3).
* **Schema registration.** A semantic operation (`register_schema`) that
  points at the `json-schema` output of a verified Blob.
* **UI.**
  * The file detail pane gets "Treat as manifest…" (choose extractor and
    schema; this creates a Verify request for that file).
  * A Manifests tab lists manifests and their records. Records can be
    filtered with JSON paths (`json_extract`, `json_each`).
  * A Schemas list supports registration.

### Phase 2 — link statements

* **Endpoint.** `POST /api/link-statements` takes a JSON Lines body: the
  header line, then one line per link, as in §12.2. It runs through the
  ledger, and the whole statement is accepted or rejected.
* **Path resolution** maps §12.2's rules onto the projection. A `path` line
  resolves to Blob B only when all of these hold:
  * the current occurrence at that location is a file;
  * its `content = 'verified'` and `content_blob_id = B`;
  * its latest metadata equals `ev_*` (not *modified*);
  * `issue IS NULL`;
  * `last_seq >= observed_since` when given.

  The read behind it is `content_read_id`, and its opened observation is the
  "observation that showed the file unchanged".

  Failure reasons map directly:

  | Projection state | Reason |
  | --- | --- |
  | No current occurrence, or not a file | `location_unknown` |
  | Unidentified | `occurrence_unidentified` |
  | *Modified* | `modified_since_verification` |
  | Changed | `content_contradicted` |
  | Issue present | `evidence_unresolved` |
  | Too old | `observation_too_old` |
* **Tables.** `link_statements` and `manifest_links` as in §13.6, with
  `semantic_op_id` and `replaces_statement_id UNIQUE`. They are immutable
  except for `withdrawn_at`.
* **Corrections.** Replacement and withdrawal are semantic operations.
* **Display.** Blob views list current links per statement. An advisor
  proposes sidecar links: a verified `x.json` next to a verified `x.mp4`,
  where the JSON validates against a registered schema.

### Phase 3 — bundles

* **Declaration.** A bundle is a statement with `bundle: true`. Every line
  has a unique relative `path` (no `..`), optionally with `blob`. Lines
  without `blob` are resolved at `base` + `path`.
* **Checking.**
  * *Check bundle at…* creates a Check (or Verify) request for the base
    directory with unlimited depth. Each expected file target uses
    `compare_blob` = the entry's Blob, so a moved bundle can be checked at
    its new base.
  * After publication the report module classifies the result as `intact`,
    `plausibly intact`, `damaged`, or `unresolved`, with added files
    reported separately. The report is stored in a
    `bundle_checks(scan_id, statement_id, base_location_id, state, report)`
    table.
* **Advisor.** An advisor flags bundle occurrences that are no longer
  intact, or that changes since their last check have made stale.
* **Restore adoption** reuses the same report. The reference is the
  previous epoch's verified view (path → Blob) at a frozen publication
  sequence; the candidate is a scan of the new root before adoption. That
  needs the candidate-binding concept: scans recorded against a new epoch
  that is not yet active.

### Risks and open questions

* Q13 and Q14 are still proposals. Whole-statement rejection,
  `observed_since`, and replacements naming a different manifest should be
  confirmed before Phase 2.
* Large manifests (millions of records) need streaming validation and
  batched inserts. The 64 MiB buffer limit is a first guard.
* The `json-schema` dialect pin and the validator crate choice need a
  security review, because schemas are untrusted input.

---

## 3. Recipe: adding an extractor

### How extractors fit today

* **The trait** (`extract.rs`):

  ```rust
  pub trait StreamExtractor {
      fn name(&self) -> &'static str;
      fn version(&self) -> &'static str;
      fn config(&self) -> String;          // canonical encoding of every setting
      fn feed(&mut self, data: &[u8]);     // bytes in file order
      fn finish(&mut self) -> Option<Value>;
  }
  ```

* **Registration.** `extract::default_stream_extractors()` lists the
  extractors that run.
* **Execution.** `Run::stream` (`scan.rs`) feeds every chunk of every full
  read to them, alongside BLAKE3 and the sample token. Afterwards it calls
  `finish` only if the read had no I/O error.
* **Admission.** `Run::admit` stores each output in `derived_metadata`,
  keyed by `(blob, name, version, config)`, in the same savepoint as the
  content result, and only when the read is accepted (stable, complete,
  recognized). If a later run with the same identity produces a different
  payload, the first result is kept and an `extractor_output_conflict`
  diagnostic is recorded.
* **Display.** The Blob panel in the UI shows every output's JSON.
  `query::format_label` turns `sniff` output into the *Content* column's
  format text.

### Rules every extractor must follow

1. **A function of the bytes.** No file name, path, time, environment, or
   network. The same bytes, name, version, and config must always give the
   same output. Otherwise the second run records a conflict.
2. **Identity.** Bump `version` whenever the output for some input could
   change. Put every output-affecting setting in `config`, in canonical form
   (for example sorted JSON).
3. **Bounded and cheap.** Use constant memory or a stated cap. Until
   request-level selection exists (Phase 0 above), streaming extractors run
   on *every* full read, including Check's full reads of small files.
4. **Privacy (§4.1).** Outputs stay inside the catalog. Never emit a
   content digest or anything matchable against public hash lists (a
   SHA-256 of the file, for example) into fields the API returns. If such a
   value is needed for an export, it belongs to an explicit export command,
   not to derived metadata shown in the UI.
5. **Report, don't suggest.** Extractors produce data. Suggestions come
   from advisors reading that data.

### Adding a streaming extractor

Example: `textstats`, which reports line counts and whether a text file is
valid UTF-8.

1. **Implement the trait** in `extract.rs` (or a new module):

   ```rust
   #[derive(Default)]
   pub struct TextStats {
       lines: u64,
       nul: bool,
       invalid_utf8: bool,
       carry: Vec<u8>, // incomplete UTF-8 sequence from the previous chunk
   }

   impl StreamExtractor for TextStats {
       fn name(&self) -> &'static str { "textstats" }
       fn version(&self) -> &'static str { "1" }
       fn config(&self) -> String { "{}".into() }

       fn feed(&mut self, data: &[u8]) {
           self.lines += data.iter().filter(|&&b| b == b'\n').count() as u64;
           self.nul |= data.contains(&0);
           if !self.invalid_utf8 {
               let mut buf = std::mem::take(&mut self.carry);
               buf.extend_from_slice(data);
               match std::str::from_utf8(&buf) {
                   Ok(_) => {}
                   Err(e) if e.error_len().is_none() => self.carry = buf[e.valid_up_to()..].to_vec(),
                   Err(_) => self.invalid_utf8 = true,
               }
           }
       }

       fn finish(&mut self) -> Option<Value> {
           if self.nul { return None; } // binary: no output rather than a misleading one
           Some(json!({"lines": self.lines, "utf8": !self.invalid_utf8 && self.carry.is_empty()}))
       }
   }
   ```

   Returning `None` from `finish` means "nothing to say for this Blob"; no
   row is stored. The example copies each chunk into `buf` for brevity. A
   production version should validate in place and keep only the trailing
   incomplete sequence.

2. **Register it** in `default_stream_extractors()`. Once Phase 0 lands,
   register it in `extract::registry()` instead, either in the default set
   or request-only.
3. **Test chunking invariance.** Add a unit test that feeds the same bytes
   in several chunk sizes and asserts identical output, the way `sniff` is
   tested.
4. **Test it end to end.** Add an integration test in
   `tests/semantic.rs` (or a new file) that verifies a tree and asserts the
   `derived_metadata` rows. Re-verify and assert that nothing new is stored.
5. **Surface it, if useful.** The Blob panel shows it automatically. For a
   table column, add a lookup in `query::entries` next to the `sniff`
   format lookup, and a field in `EntryView` and `web/src/types.ts`.
6. **Document it.** Record the extractor in IMPLEMENTATION.md (section 1.10) and
   its cost in the defaults table.

### Adding a seeking extractor: thumbnails (planned)

A thumbnailer needs the whole file through a seekable handle and often an
external decoder, so it cannot be a streaming extractor. The framework
below implements §4.2's second attachment point and the
`with-blob` idea in IDEAS.md.

**1. Trait.**

```rust
pub struct ArtifactOut { pub media_type: String, pub bytes: Vec<u8> }

pub trait SeekingExtractor {
    fn name(&self) -> &'static str;
    fn version(&self) -> &'static str;   // include the external tool's version
    fn config(&self) -> String;
    /// Decide from the streaming outputs of the same read (e.g. `sniff`).
    fn applies(&self, size: u64, streamed: &[(&str, &Value)]) -> bool;
    /// Run against the already-hashed handle. Must not modify anything.
    fn run(&self, file: &File, deadline: Instant) -> Result<(Value, Option<ArtifactOut>), String>;
}
```

**2. Call site.** In `Run::stream`, at the existing comment "Seeking
extractors would run here", after the read loop and *before* the final
`fsx::meta_of` stability check, so the handle comparison covers the
extractor's time as §3.4 requires:

* look up the Blob by digest (a read-only `SELECT`; the Blob may not exist
  yet);
* skip an extractor whose `(blob, name, version, config)` result already
  exists (§4.2);
* otherwise run it with a deadline and add its output to `Stream.outputs`.

A failure or timeout records a diagnostic and adds no output. The content
result is unaffected. Because no write transaction is open during large
reads (`LARGE_READ`), slow extractors do not hold the database.

**3. External tools without trusting paths.** Never pass the file's path:
it may have changed since the read. Instead, duplicate the open handle into
the child and pass `/dev/fd/3`:

```rust
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;

let fd = file.as_raw_fd();
let mut cmd = std::process::Command::new("pdftoppm");
cmd.args(["-png", "-singlefile", "-scale-to", "256", "/dev/fd/3", "-"])
   .stdout(std::process::Stdio::piped());
// SAFETY: dup2 and fcntl are async-signal-safe; this runs in the child before exec.
unsafe {
    cmd.pre_exec(move || {
        // dup2 onto itself would keep FD_CLOEXEC, so clear it explicitly in that case.
        let r = if fd == 3 { libc::fcntl(3, libc::F_SETFD, 0) } else { libc::dup2(fd, 3) };
        if r < 0 { Err(std::io::Error::last_os_error()) } else { Ok(()) }
    });
}
```

Opening `/dev/fd/3` in the child reaches the same inode even if the path
was renamed meanwhile, and the child gets its own file offset. Kill the
child at the deadline (poll `try_wait`, then `kill`). Run decoders
sandboxed, for example under `bwrap --unshare-all --die-with-parent` with
read-only system binds and no network, because decoders parse untrusted
input. Possible tools:

| Format (from `sniff`) | Tool |
| --- | --- |
| PDF | `pdftoppm -png -singlefile -scale-to 256 /dev/fd/3 -` |
| Images | `magick /dev/fd/3[0] -thumbnail 256x256 -strip png:-`, or the `image` crate in-process for common formats |
| Video | `ffmpeg -nostdin -i /dev/fd/3 -vf thumbnail,scale=256:-1 -frames:v 1 -f image2pipe -vcodec png -` |

**4. Artifact storage**, following the direction in IDEAS.md:

* Small JSON stays in `derived_metadata.payload`.
* Artifacts are written to
  `<catalog>/artifacts/<blob public id>/<name>-<version>-<blake3(config)[..8]>.<ext>`.
  The write is atomic (temp file plus rename) and happens before the result
  savepoint commits. The path is named by Blob ID, never by digest, and it
  stays inside the catalog's privacy boundary.
* The payload records `{"artifact": "<relative path>", "media_type": …,
  "size": …, "blake3": …}`. The artifact hash lets a re-run detect
  nondeterministic output as an `extractor_output_conflict`.
* Make output reproducible. `-strip` and PNG chunk exclusions remove
  timestamps that encoders otherwise embed.
* Orphaned artifacts (written before a crash rolled back their row) are
  garbage. A startup sweep removes files with no referencing row.

**5. API and UI.**

* `GET /api/blobs/{id}/artifacts/{name}` looks up the payload through the
  worker and streams the file from the catalog directory. It never reads the
  managed tree.
* The Blob panel and the Browse detail pane show the thumbnail. An optional
  thumbnail column in Browse can load images lazily.

**6. Selection and backfill.**

* Thumbnails are request-only, not default (Phase 0): Verify with
  `extractors: [{"name": "thumbnail", "config": "{\"size\":256}"}]`.
* For content verified before the extractor existed, an advisor proposes
  "Verify N files whose content lacks a thumbnail". This is the
  `extract --missing` idea in IDEAS.md, expressed as an ordinary Verify of
  selected files, so it reuses the read procedure, stability checks, and
  exclusive access.

**7. Tests.**

* Fake-tool tests use a shell script that writes a fixed PNG, to check:
  * the call site and stability coverage;
  * skip-if-present;
  * timeout handling (a script that sleeps);
  * conflict detection (a script that varies its output).
* A file modified while the tool runs yields an unstable read and no stored
  artifact.
* An integration test with `pdftoppm` is skipped when the tool is absent.

**Cost.** Thumbnailing is far slower than hashing, so a thumbnail Verify of
a large archive should be scoped (by directory, or by format via an advisor)
and cancellable between files, as all scans are. Starting one process per
file costs about 5–50 ms. Formats that the `image` crate handles are better
done in-process.
