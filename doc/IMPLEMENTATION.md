# Filestore prototype: implementation notes

This document records how the prototype in `crates/` and `web/` relates to
[DESIGN.md](DESIGN.md). It covers where the implementation deviates from the
proposal, how it handles situations the proposal does not address, and the
other design decisions made along the way. For each one it also describes
the effect on use cases and day-to-day operation.

Terms follow DESIGN.md (Blob, Location, Observation, Occurrence, Inventory,
Verify, Check), and "§" always refers to a section of DESIGN.md. Sections of
this document are referred to by number alone ("see 1.2"). Code references
are relative to `crates/filestore/src/` unless stated otherwise. The implemented schema is drawn in
[SCHEMA.dot](SCHEMA.dot), which is generated from `schema.sql` by
[schema_dot.py](schema_dot.py).

Each deviation is labeled:

| Label | Meaning |
| --- | --- |
| **Equivalent** | Same guarantee, different mechanism. |
| **Simplified** | A narrower or weaker guarantee; the limitation and what eventually reveals it are described. |
| **Omitted** | Not implemented. |
| **Extension** | Behavior the proposal does not specify. |

## Contents

1. [Deviations from the proposal](#1-deviations-from-the-proposal)
2. [Situations the proposal does not cover](#2-situations-the-proposal-does-not-cover)
3. [Other design decisions](#3-other-design-decisions)
4. [Defaults and limits](#4-defaults-and-limits)
5. [Measured performance](#5-measured-performance)

---

## 1. Deviations from the proposal

### 1.1 Sampling policy (§6.3) — Simplified

**Proposal.** Check reads a byte budget anchored at 80% of a 1 MiB file and
growing logarithmically above it, in 16 KiB blocks. Interior blocks are
chosen by Floyd's algorithm from a BLAKE3 key-derivation stream with
rejection sampling. Files below the anchor are sampled as well, and need a
baseline.

**Prototype** (`sample.rs`). The policy `filestore-sample/1;block=65536;count=16`
works as follows:

* With `N = ceil(size / 64 KiB)`: if `N ≤ 16` (files up to 1 MiB), the plan
  covers the whole file and Check performs the full Verify procedure.
* Otherwise exactly 16 blocks are read, at indices `j·(N−1)/15` for
  `j = 0..15`. These are evenly spaced and always include the first and last
  block.
* The token keeps the proposal's framing:
  `BLAKE3::derive_key("filestore sample token v1", …)` over the policy
  encoding, the size, the block count, and each block's offset, length, and
  bytes. Streamed (Verify) and block-wise (Check) computation give the same
  token, which `sample.rs` tests.
* Baselines are stored only for sampled-plan sizes (> 1 MiB). Smaller files
  need none because Check reads them fully.

**Why.** The proposal's algorithm costs a lot of specification and test
vectors (Floyd selection, rejection sampling, logarithmic bands) for little
practical benefit in a non-adversarial archive. Both schemes have fixed,
public positions and the same blind spot. A fixed count bounds the cost of a
sampled Check at 1 MiB of reads and 16 seeks per file, at any file size.
That matters on spinning disks, where the proposal's 132 seeks for a 1 GiB
file dominate the read time.

**Impact.**

| File size | Proposal reads | Prototype reads |
| --- | --- | --- |
| ≤ 1 MiB | ~80% sampled; baseline required | 100% (a Verify; no baseline) |
| 4 MiB | 68 blocks, 1.06 MiB | 16 blocks, 1 MiB |
| 1 GiB | 132 blocks, 2.06 MiB | 16 blocks, 1 MiB |
| 1 TiB | 212 blocks, 3.31 MiB | 16 blocks, 1 MiB |

* Small files are always exactly verified by Check. Archives with many small
  files therefore get stronger evidence at a small extra cost: at most 20%
  more bytes for those files.
* Large files get thinner coverage than in the proposal. An edit confined to
  unsampled bytes keeps the size and escapes Check (*plausible*) in both
  designs; it is somewhat likelier here for multi-GiB files.
  `check_matches_samples_and_misses_unsampled_edits` in `tests/scan.rs`
  pins this blind spot down.
* Evenly spaced positions can miss periodic edits with exactly the stride
  period. Random positions have no such pattern, but they offered no
  practical advantage for a non-adversarial archive.
* The policy is versioned by its encoding. A different block size or count
  produces new, incompatible baselines, and Check then reports `no_baseline`
  until a Verify. There is no setting to change it yet.

### 1.2 Location checks compare only the final object (§3.4 step 5) — Simplified

**Proposal.** After reading, re-resolve the location under the root and
check that *its components and final entry* still denote the observed
objects.

**Prototype** (`Run::location_check`, `fsx::locate`). The full path is
re-resolved from the open root handle with a single `openat2(O_PATH)` using
`RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_XDEV`, and only the
final object's device and inode are compared with the opened handle.
Intermediate directories are not compared with the anchors or listings that
led to the file. The same applies to the location check after a directory
listing.

The projection compounds this. `Projector::apply_obs` attaches an
observation to whichever directory occurrence is current at the parent
*location*. It does not use the observation's recorded `parent_obs_id`.

**Consequence.** Take a directory `d` that is listed by a scan, then
replaced by a new directory before the scan opens `d/f`, with the same file
object moved into the replacement:

1. The read succeeds, the location check confirms (same inode at
   `d/f`), and the file is recorded as *verified* under the old instance
   of `d`, the one this scan listed.
2. Any later scan that observes `d` sees a different inode and ends the
   old instance together with its descendants (`ancestor_ended`). That
   includes an Inventory of its parent, any listing of `d`, and a file
   target below `d`, whose ancestors are re-anchored at the start of
   every scan.
3. `d/f` then becomes a new, *unidentified* occurrence. Its previous
   content is remembered as the path's last verified Blob, so Check
   compares against it (*plausible*, or *verified* for files up to
   1 MiB) and Verify re-establishes it.

If the replacement holds a different object at `d/f`, step 1 instead starts
a new occurrence: its inode differs from the listing entry. It still sits
under the old directory instance, and steps 2–3 correct it the same way.
`tests/limitations.rs` reproduces the scenario with a real directory swap.

**Impact.**

* The content evidence is always about the right object: the one actually
  opened at that path and read stably. Only the *directory-instance* lineage
  is wrong, and only for the interval between the swap and the next
  observation of the parent.
* No absence claim is affected. Absence comes only from complete, stable,
  location-confirmed listings, and those do check the listed directory
  itself.
* An ABA sequence (swap, read, swap back before the next scan) is
  undetectable. Per-component checks would not catch it either if the
  intermediate state happened between two checks.
* To make this compliant: walk the path component by component with
  `openat(O_PATH | O_NOFOLLOW)` and compare each directory's inode with the
  anchor chain (or the listing's `dir_obs`). Then make the projection attach
  through `parent_obs_id` instead of the parent location's current
  occurrence. Both changes are local to `scan.rs` and `project.rs`.

### 1.3 Root bindings, activation, and restore comparison (§3.3) — Simplified / Omitted

**Proposal.** Root bindings (candidate or active), an append-only
activation chain, candidate scans published under their own binding, and a
frozen reference view with an exception report (changed, missing,
unresolved, and added counts; 1% and 20-exception thresholds) reviewed
before adoption. There is also an explicit "restore" action for in-place
restores that keep the root's identity.

**Prototype.**

* **One implicit binding.** The root path is stored relative to the catalog
  (§3.1), and `epochs` records the filesystem context and root inode. The
  active epoch is the latest row.
* **Identity check.** Every scan compares the root against the active epoch
  when it starts and again before publishing. A mismatch fails the scan,
  publishes nothing, and stores `pending_root_change` in `meta`. The UI shows
  a banner, and scans stay blocked until the user adopts the change.
* **Adoption.** `scan::adopt_root_identity` appends a new epoch, recorded as
  an `adopt_root_identity` semantic operation. If the root returns to its
  old identity, the pending change clears on the next scan.
* **Omitted:** candidate bindings, the reference view, the exception
  report, thresholds, and the explicit restore action.

**Impact by scenario.**

| Scenario | What happens |
| --- | --- |
| Reboot, remount, disk moved to another machine (same filesystem) | Same context and root inode: the epoch continues; listings and verification ages keep their meaning. |
| Archive copied or restored onto a different filesystem, or root directory recreated | Scans fail with *root replaced* or *filesystem context changed* until adoption. After adoption every location starts unidentified in the new epoch; Check compares each path with the last content verified there in any epoch (plausible for large files, verified for files ≤ 1 MiB), and Verify recovers Blob IDs with their tags and revisions. |
| In-place restore that keeps the root inode (e.g. `rsync` into the existing tree) | Indistinguishable from ordinary edits. Files rewritten with new inodes become new occurrences (Check compares with the path's previous Blob); files rewritten in place show *modified?*. Nothing is wrongly reported as verified, but there is no single "restore" report. |
| Wrong disk mounted at the root path | Detected as a context change; nothing is published; the user must not adopt it. The adoption dialog shows both identities but offers no content comparison to help decide. |

Without the comparison report, deciding to adopt relies on the user's
judgement. The workaround is to adopt, run Check, and review the results.
After adoption, though, nothing distinguishes "restored correctly" from
"different archive" except those Check results. The report is planned
together with bundles ([PLANS.md](PLANS.md#2-manifests-link-statements-and-bundles)),
because both compare a tree with an expected path→Blob mapping.

### 1.4 Filesystem comparison context (§3.3, Q10) — Equivalent, with a caveat

**Prototype** (`fsx::fs_context`, `fsx::same_context`). The context string
always contains the filesystem type and the `statfs` filesystem ID. It adds
`uuid=` when `FS_IOC_GETFSUUID` works (Linux 6.7+, filesystem-dependent) and
`subvol=` when `STATX_SUBVOL` does (Linux 6.10+). Contexts are compared
structurally:

1. The filesystem types must match.
2. When both sides have a UUID, it decides; otherwise the statfs IDs must
   match.
3. When both sides know the subvolume, it must match.
4. Parts present on only one side are ignored, so kernel upgrades and
   downgrades do not look like a different filesystem.

**Caveat.** XFS derives its statfs ID from the device number. On kernels
without `FS_IOC_GETFSUUID`, renumbering a device between boots (for
example, a USB disk enumerated in a different order) looks like a
different filesystem and blocks scans until adoption, which costs listing
coverage and verification ages. ext4 and btrfs derive the ID from the
filesystem UUID and are unaffected.

### 1.5 Scan requests, sessions, and targets (§5.1, §13.1, Q15) — Simplified

* **Request and session are one row.** A `scans` row is both the durable
  request and its single session, matching Q15's proposed direction: one
  session per request; a failed or abandoned scan finishes its request; a
  re-run is a new request.
* **Queued requests survive restarts.** The server re-enqueues them at
  startup. A queued request can be cancelled; it then closes with the extra
  outcome `cancelled` (Q15).
* **No request-level options.** The sampling policy, retry limit, and
  extractors are fixed; the session records what was used in
  `scans.policy`. There is no per-target `comparison_blob_id` override, so a
  file cannot be checked against a Blob the user names. That override is
  needed for "check this candidate after a suspected move" and for bundles
  (see [PLANS.md](PLANS.md)).
* **Target normalization** (`scan::normalize_targets`) removes duplicates
  and targets covered by a directory target of sufficient depth. A file
  target at the *same* path as a directory target is kept.
* **Target results** live in `scans.summary.targets` (path, status
  `done | finished_with_gaps | failed | not_started`, reason) rather than in
  a separate table.

### 1.6 Attempt recording and durability (§9.1–9.2) — Equivalent for published evidence

* **Listings are written as one unit.** A listing is buffered in memory
  (names plus one `statx` per entry) and written as attempt, opened
  observation, entries, and result in a single transaction. Inserting
  the result closes the attempt (an `observations` trigger rejects later
  entries), so closure is still enforced. If the process crashes
  mid-listing, the listing leaves no row at all rather than an unfinished
  attempt.
* **Reads insert the attempt and the opened observation before hashing.**
  The result, Blob admission, baseline, and extractor outputs are written in
  one savepoint afterwards, as the proposal requires.
* **Batching.** Consecutive attempts share one write transaction for up to
  250 ms (`BATCH_INTERVAL`). Each attempt's own atomic section nests inside
  it as a savepoint. Before hashing a file of 8 MiB or more (`LARGE_READ`)
  the batch is committed, so no transaction stays open during long reads.

  The reason: one commit per attempt cost about 33 WAL page writes per file
  and capped Verify at about 1,200 small files per second. Batching more
  than doubled throughput (section 5).

  The cost: a crash loses up to 250 ms of *unpublished* attempts. Abandoned
  scans are never resumed or published, so no published evidence is lost;
  the diagnostics of the abandoned scan are just slightly less complete.
* **No selection tables.** `selected_directory_results` and
  `selected_file_results` are not needed. Publication makes every terminal
  attempt of the scan an input to the projection, which is what the
  proposal's selection rows had to express anyway: "include all terminal
  attempts, including failures".

### 1.7 Closure and immutability (§9.4) — Equivalent

Triggers in `schema.sql`:

* Evidence rows (observations, enumerations and their results, reads and
  their results, diagnostics, derived metadata) can be inserted only while
  their scan is `running`.
* Entries cannot be added to an enumeration that has a result.
* A closed scan row cannot be updated.
* Evidence tables, `blobs`, `baselines`, `locations`, `epochs`, and
  `sampling_policies` reject `UPDATE` and `DELETE`.
* `revisions.blob_id` and `revisions.lineage_id` are immutable.
* Finalization checks that it closed exactly one running scan before
  applying the projection.

**Code-enforced, not schema-enforced:**

* A Blob's `admitted_from_read_id` is an accepted full recognition (the
  result row is inserted after the Blob in the same savepoint).
* At most one publication per scan and the uniqueness of
  `publication_seq` (a UNIQUE column).

### 1.8 The projection (§7, §13.7) — Simplified

* **One materialized table.** `occurrences` holds one row per occurrence.
  Rows for files and directories are the occurrences and directory instances
  of the proposal. It is updated in the publication transaction and can be
  rebuilt from published evidence (`project::rebuild`, `filestore rebuild`).
  `project::snapshot` exists so tests can assert that rebuilt and
  incremental projections are identical.
* **Only the latest publication is materialized.** There are no as-of views
  ("the tree as of publication #12") and no cache keyed by interpretation
  version. `INTERPRETATION_VERSION` is declared but not yet used. Changing a
  projection rule requires `filestore rebuild` on existing catalogs.
* **Continuity.** The same epoch, kind, and inode, under the current parent
  occurrence (see 1.2), means the same occurrence. Inode generation
  numbers are not recorded: reading them needs an ioctl per file, and
  btrfs, the development filesystem, does not reuse inode numbers in
  practice. On ext4 an inode reused within one epoch can therefore look like
  continuity. The next content check then shows a size or sample mismatch
  rather than silently accepting different content.
* **Listing freshness (§7.2).** A directory's listing is *stale* when a
  later observation of that directory shows an mtime or ctime different from
  the "after" metadata of its last complete listing.
* **"modified?"** A verified or plausible file whose current size, mtime,
  or ctime differs from the read that produced its content evidence is shown
  as *modified?* (`query::file_status`). Including ctime means `chmod`,
  `chown`, and new hard links also show as *modified?*. They lead to a Check
  suggestion, which then confirms the content cheaply. This is noisy but
  conservative.
* **Event order.** Root and anchor observations are applied at their event
  order. Listings and reads are applied as units at their start order:
  opened observation, then entries, then result. Attempts are strictly
  sequential, so this is equivalent to terminal order.

### 1.9 Check comparison targets (§6.1) — Simplified, with one extension

`Run::comparison_target` chooses the Blob to compare against *after*
opening the file, from the projection as of the last publication:

| Situation at the location | Comparison target |
| --- | --- |
| Opened inode = current occurrence, content verified or plausible | that Blob (`occurrence`) |
| Opened inode = current occurrence, content *changed* | none: `no_baseline` with reason "ruled out; Verify" (no purposeless sampling) |
| Opened inode ≠ current occurrence (replacement since the last scan) | the path's last known content: the occurrence's verified/plausible Blob, else its last verified Blob, else its predecessor's (`location_history`) |
| No current occurrence (new path, or a new epoch) | the last verified Blob at this path in **any** epoch (`location_history`) |

Comparison targets are never found through inode matches at other paths.
The extension is the cross-epoch lookup: the proposal allows comparing with
"the last verified Blob at that root-relative location". That is what makes
post-restore Checks useful (1.3).

**Omitted:** comparing against a user-named Blob (see 1.5).

### 1.10 Extractors (§4.2) — Simplified

* **One built-in extractor.** `sniff` (format detection from the first
  8 KiB) is the only extractor. It is streaming and runs on **every** full
  read: Verify reads, and the full reads Check performs for files up to
  1 MiB.

  The proposal says extractors run only when a Verify request names them,
  and never during Check. `sniff` is cheap and a pure function of the
  bytes, so running it everywhere is harmless and keeps format information
  available for all content. Expensive extractors must not follow this
  pattern (see [PLANS.md](PLANS.md#3-recipe-adding-an-extractor)).
* **No `scan_extractions` or `extractor_runs` tables.** An output row in
  `derived_metadata` records its scan and read. The identity
  `(blob, extractor, version, config)` is unique, and a differing re-run
  records an `extractor_output_conflict` diagnostic and keeps the first
  result.
* **Omitted:** seeking extractors (the attachment point is marked in
  `Run::stream`), retraction, and backfill outside a scan.

### 1.11 Advisors and accepted-action provenance (§10, §13.5) — Simplified

* **Computation.** Suggestions are computed on demand from the projection
  (`advise.rs`). They read no file content and write nothing; a test checks
  this.
* **Identity and staleness.** A suggestion's ID is a hash of its advisor and
  *action*. Acceptance recomputes all suggestions and proceeds only if the
  same ID is still offered; otherwise it returns 409.

  As a result, an identical action is accepted even if its supporting
  evidence changed since display. The provenance snapshot is the
  *recomputed* suggestion, so it reflects the evidence at acceptance rather
  than exactly what was on screen. The proposal asks for revalidation, which
  this does. A stricter variant would also compare `input_seq` and reject
  any change.
* **Provenance storage.** `action_provenance` stores the snapshot as JSON,
  evidence included. There is no `action_evidence` table of typed
  references. Scan requests reference provenance through
  `scans.provenance_id`, and semantic operations through
  `semantic_ops.provenance_id`.
* **Dismissal** lasts only for the browser session (`sessionStorage`).
  Dismissed suggestions return in a new session.

The advisors and their heuristics:

| Advisor | Proposes | Notes |
| --- | --- | --- |
| `first-scan` | Inventory of the root | Only before the first scan. |
| `read-failures` | Verify (retry) | Lists the causes (EACCES…); priority below content changes because retrying a permission error is pointless until access is fixed. |
| `changed-content` | Verify | Content ruled out by a size or sample comparison. |
| `modified-files` | Verify if the size differs, else Check | |
| `listing-gaps` | Inventory: unlimited depth for never-listed or replaced directories, depth 0 for stale or failed listings | |
| `possible-moves` | Verify files / Inventory directories | An ended occurrence whose inode appears at another path afterwards. Those paths are excluded from the generic suggestions. A recursive Inventory lists a renamed directory fully, so a directory rename usually surfaces as per-file move suggestions. |
| `unidentified-files` | Verify | Compacted to a directory target (depth 0) when every file of a directory needs it. |
| `plausible-only` | Verify | Files known only by sample match, e.g. after a restore. |
| `overdue` | Verify | Verification older than the horizon (default 730 days). |
| `lineage-successor` | Create a Lineage or add a revision | A path verified as Blob A and later as Blob B, now verified as B, with no existing revision B after A. At most 50. |
| `name-sequence` | Create a Lineage | Files in one directory whose names are equal after removing bracketed parts and replacing digit runs with `#`, the stem keeping at least 6 letters. Groups of 2–20 distinct Blobs, none already in a Lineage, ordered by natural sort. At most 100. A heuristic: it also proposes "Part 8 / Part 9"-style series. |

### 1.12 Command acceptance (§10.1) — Simplified

**Prototype.** There are no server-issued witnesses. A mutating request may
carry a client-generated `request_id`. `ledger::run` stores the operation, a
hash of the canonical JSON payload, and the result in the same savepoint as
the operation. An identical retry returns the stored result; reusing an ID
with a different payload is rejected (409). A failed first attempt records
nothing.

Only scan submission, suggestion acceptance, Lineage creation, and adding a
revision go through the ledger. Tag operations are naturally idempotent or
protected by uniqueness, and renames are idempotent.

**Impact.** Safety depends on the client reusing the same ID for retries
(the web UI generates one per user action and retries once with it). A
client that mints a new ID per retry could create duplicates. A witness
issued by the server would prevent that; for a single local UI the client
ID suffices.

### 1.13 Semantic records (§11) — Simplified

* **Tags.**
  * Each hierarchy is an exclusive tree. Top-level tags have no parent: the
    hierarchy itself acts as the single root the proposal asks for.
  * Tag names are exact, case-sensitive Unicode, unique among siblings, and
    may not contain `/`, which display paths use as separator.
  * Tags can be created and renamed. Reparenting, tombstoning, and merging
    are omitted.
  * One `tag_assignments` table with a `target_kind` column replaces the
    three per-target tables. The primary key enforces one tag per target and
    hierarchy; the polymorphic `target_id` has no foreign key (dashed in the
    diagram).
* **Unidentified content cannot be tagged** (§11). The API takes Blob IDs,
    and a Blob exists only after a full recognition. The UI additionally
    offers tagging only for *verified* files: for a *plausible* or
    *modified?* file it would be tagging the expected content, not
    necessarily what the file now holds.
* **Lineages and revisions.**
  * Revision parents form a DAG within one Lineage. Creating a revision
    cannot create a cycle; parent edits are checked against descendants.
  * The same Blob may appear repeatedly in a Lineage. Revisions and
    Lineages are tombstoned, never deleted.
  * `revision_sources` (the observations that motivated or manifested a
    revision) is omitted. When a revision came from a suggestion, its
    origin is in `action_provenance`; otherwise it is not recorded.
* **Operation log.** Every semantic change writes a `semantic_ops` row with
  a JSON array of `{entity, before, after}` changes in the same transaction,
  instead of `semantic_changes` rows. The actor is always `local-user`.
* **Root adoption** is logged as a semantic operation. The proposal treats
  binding activation as its own record kind.
* **Assignment conflicts.** The proposal wants different selections from
  one hierarchy across Blob, Lineage, and revision scopes shown as
  conflicting contextual assignments. The prototype shows each scope's
  assignments where that object is displayed, but has no combined view that
  highlights such conflicts.

### 1.14 Identifiers and paths — Extension

* **Public IDs** are 128 random bits in lowercase base32 with a type prefix
  (`blob_`, `scan_`, `lin_`, `rev_`, `tag_`, `hier_`, `op_`, `epoch_`,
  `archive_`) rather than 256-bit values. 128 bits is ample for
  unguessability and uniqueness, and the prefix makes IDs self-describing in
  logs and the UI.
* **Locations.** `locations` interns every root-relative path as a byte
  string with its parent and name. Observations reference a location rather
  than deriving paths from observation ancestry; `parent_obs_id` is still
  recorded.
* **Text form of byte paths** (`pathenc.rs`), which the proposal left open.
  Valid UTF-8 passes through, `%` becomes `%25`, and any invalid byte
  becomes `%XX`. It is lossless and readable for ordinary names.

### 1.15 Verification freshness (§6.4) — Simplified

**Prototype.**

* **Horizon setting.** One setting, `verify_horizon_days` (default 730).
* **Overdue status.** A file is *overdue* when it is verified and its last
  verification is older than the horizon. Overdue is derived at query time,
  not stored.
* **No score curve.** There is no freshness score; dates and the overdue
  flag are shown instead.
* **Plausible files** are not marked overdue; they have their own Verify
  suggestion.
* **Overdue counts** in subtree aggregates refresh at least hourly.

### 1.16 Process model and exclusive access (Q16) — Equivalent

**Prototype.**

* **One server process.** It holds an exclusive `flock` on
  `<catalog>/lock` for its lifetime. CLI commands take the same lock, so
  they cannot run while the server holds the catalog.
* **One worker thread.** Inside the process, a single worker thread owns
  the SQLite connection and runs every job — queries, semantic operations,
  and scans — in FIFO order. A scan therefore has exclusive access, and
  requests submitted during it wait.
* **What bypasses the worker.** `/api/status`, scan progress, and
  cancellation use shared memory and never touch the database.
* **The UI during a scan.** It keeps showing already loaded data and defers
  background refreshes until the scan ends. Proposal §9.1: "a frontend may
  display already loaded data with its original age."

---

## 2. Situations the proposal does not cover

### 2.1 Scan lifecycle

| Situation | Behavior | Impact |
| --- | --- | --- |
| User cancels a running scan | The scanner stops before the next attempt. It records a `cancelled` gap and publishes what it completed as `finished_with_gaps`. | Long Verify runs can be stopped without losing finished work. Cancellation waits for the **current file** to finish: a single 100 GiB file cannot be interrupted mid-read. |
| Server receives Ctrl-C or SIGTERM | The running scan is cancelled; the server waits up to 30 s for it to finalize, then exits. | A clean stop publishes partial work. If the current file takes longer than 30 s, the process exits and the next start records the scan as *abandoned*. |
| A worker job panics | The panic is caught; any open batch is rolled back; a running scan is closed as `failed` (unpublished); the worker continues. | One bad file cannot take the server down or leave a scan `running` forever. |
| Requests submitted while a scan runs | The HTTP request waits until the worker reaches it (after the running scan), then the new scan runs. | Queuing several scans from the UI works, but each request appears to hang until the previous scan finishes. |
| Queued scans at shutdown | Kept and re-enqueued at the next start. | A restart does not lose requested work. |
| Root replaced or unmounted *during* a scan | Detected before publishing (root re-opened and compared); the scan fails unpublished. Admitted Blobs stay (§9.3). | Matches §9.2. A replace-and-restore within one scan (ABA) is not detected. |

### 2.2 Filesystem behavior

| Situation | Behavior | Impact |
| --- | --- | --- |
| Bind mounts from the same filesystem (same `st_dev`) | Entries are measured with `statx`; `STATX_ATTR_MOUNT_ROOT` marks them as boundaries. | Recorded as a boundary, not entered, and no repeated listing failures. |
| `openat2` returns `EAGAIN` (concurrent rename during constrained lookup) | Retried up to 3 times, then reported as an error. | Rare transient failures do not become gaps. |
| Timestamps beyond the years 1677–2262 | Nanosecond conversion saturates. | No overflow or panic. Such timestamps compare equal to each other once clamped, so changes between two out-of-range times go unnoticed. |
| A file grows while it is read | Reading stops once past the opened size; the read is *unstable* and retried once. | Files being written (downloads, logs) produce an `unstable` gap instead of a wrong Blob. |
| A FIFO, socket, or device raced into a file's place | Files are opened with `O_NONBLOCK`; the handle's type is checked; the result is `not_regular`. | No hang, and nothing is read from special files. |
| Duplicate names within one `getdents` listing | The listing is `incomplete` (retried once if stable). | A racing directory never yields false absence. |
| An entry's `statx` fails but the name was listed | The entry is recorded with its error; the listing stays complete. | Name coverage survives per-entry failures. |
| Unreadable directory | The listing attempt fails (`open failed: EACCES`); the earlier listing stays as last-known with a listing issue; its children are not visited. | Shown as a failed listing, never as empty. |
| A listed directory that cannot be confirmed at its path afterwards | Its entries are not processed (gap `descendants_skipped`). | Work is never anchored to an object no longer known to be at that path. |
| A symlink or file where a target's ancestor directory should be | Resolution fails at that component (`target_unresolved` gap, with the anchor observation of what is there). | Targets never follow symlinks; the failed component is reported. |
| Hard links | Each path is its own occurrence, and each must be verified separately. The detail pane lists other paths with the same inode as "not proof". | Matches the proposal (§3.2); a Verify of one link does not verify the other. |
| Empty files and identical copies | All share one Blob each: every empty file is the same Blob. | Tags attach to content: tagging one empty file tags all of them. Intended by the design, but surprising in practice. |
| Reading updates atime | Not compared; reads never change mtime or ctime. | Scans cause no self-inflicted *modified?* states. `O_NOATIME` is not used, because it requires file ownership. |

### 2.3 Evidence edge cases

| Situation | Behavior | Impact |
| --- | --- | --- |
| Which later evidence resolves an earlier failure? | A full recognition resolves everything. A confirmed sample or size comparison resolves `open_failed`, `unstable`, and `location_*` issues. `no_baseline` resolves only `open_failed` and `location_*`. A mid-read I/O error (`unreadable`) stays until a full read succeeds, because samples can miss the failing region. | Restoring permissions and running Check clears the earlier access error; a bad sector is not hidden by a lucky sample. Covered in `tests/regressions.rs`. |
| Integrity errors (digest match with a different size, or a baseline conflict) | The scan fails unpublished, with an error diagnostic. | Integrity problems show in the Scans tab, not as a file status, because the failed scan's evidence never becomes current. |
| A file whose size crosses 1 MiB | Check compares sizes first, giving `size_mismatch` without reading. | The file shows *changed*, and Verify follows; no baseline gap. |
| Content ruled out, then restored with a **new** inode | Compared with the path's last verified content and can become *plausible* again. | Undo-by-replace is recognized. |
| Content ruled out, then edited back **in place** | `no_baseline` with "ruled out; Verify". | Only Verify re-establishes it, as §7.3 requires. |
| Directory renamed | A recursive Inventory lists the new name fully. Its files are new unidentified occurrences, each suggested as a possible move; nothing is transferred. | One Verify suggestion recovers all Blob IDs. |
| Previous-epoch occurrences | Shown as history (`previous epoch`), never as current. Blob detail lists them among locations. | After adoption the old tree stays browsable as history. |

### 2.4 Operational details

| Situation | Behavior | Impact |
| --- | --- | --- |
| Root configured through a symlink | `init` canonicalizes it; the catalog stores the resolved directory, relative to the catalog directory. | Re-pointing the symlink later does not change the catalog's root. |
| Catalog inside the root, or the root inside the catalog | Rejected at `init`. | Catalog files are never scanned as archive content. |
| Two catalogs on one root | Allowed; each has its own lock. | Read-only scanning means they do not interfere. |
| Non-UTF-8 names in search | Search compares `lower(CAST(path AS TEXT))`. | Invalid bytes match loosely; `lower()` folds ASCII only. Display and storage stay lossless. |
| Very large directories | A listing is held in memory while it is written; the directory view returns at most 20,000 entries. | Fine up to hundreds of thousands of entries per directory; the UI notes truncation. |
| Wall-clock jumps | Ordering uses publication sequence and event order, never time. | Only displayed ages and overdue status are affected. |
| No authentication | The server binds `127.0.0.1` by default. | The catalog is as sensitive as the files (§4.1); do not expose the port. |

---

## 3. Other design decisions

### 3.1 Architecture

* **Crates.** `crates/filestore` is a synchronous library with no async
  runtime. It holds the catalog, scanner, projection, queries, advisors,
  semantic operations, and command ledger.
  `crates/filestore-server` builds the `filestore` binary: CLI subcommands
  plus the Axum API. The scanner runs on the caller's thread; the server
  calls it from the worker thread.
* **One connection.** All access goes through one `rusqlite::Connection`
  with WAL, `synchronous=NORMAL`, and foreign keys on. Tables are `STRICT`,
  with CHECK constraints for enums and lengths.
* **Nestable atomic sections** (`catalog::atomic`). Implemented with
  savepoints instead of `BEGIN`, so independently atomic operations compose:
  the ledger wraps a semantic operation, and the scanner's 250 ms batch
  wraps per-attempt sections. All code uses `atomic()`, never
  `transaction()`.
* **Shared interpretation.** Status codes are computed once, in
  `query::file_status` and `query::build`, from the projection. The UI never
  derives evidence states on its own. Explanation sentences are composed
  client-side (`web/src/util.ts`) only so that relative ages stay current
  without refetching.

### 3.2 Status vocabulary

| Status | Meaning |
| --- | --- |
| `verified` | Full read with confirmed location; metadata unchanged since. |
| `modified` (shown as *modified?*) | Content was established, but size/mtime/ctime changed since; not proof of a content change. |
| `plausible` | Latest content evidence is a sample match. |
| `changed` | The expected Blob was ruled out at this location. |
| `unidentified` | Never fully read at this occurrence. |
| `listed` / `stale` / `unlisted` / `boundary` | Directory listing coverage. |
| `gone` / `previous_epoch` | History: an ended occurrence, or one from an earlier epoch. |
| `formerly_verified` | In Blob detail: a location that once held this Blob but now holds something else. |

A failure overlays any status as an issue (a red "!" in the UI). It does
not replace the status, so last-known evidence stays visible.

### 3.3 API conventions

* JSON over HTTP under `/api`, with paths in the lossless text form and
  content named only by public Blob IDs.
* Errors map to HTTP: `Invalid` → 400, `NotFound` → 404, `Conflict` → 409
  (including stale suggestions and reused request IDs), everything else →
  500.
* `GET /api/status` returns progress, the queue, catalog info, and a
  `data_version` that increments on every change. Clients poll it and
  refetch views when the version changes.
* Scan summaries are JSON in `scans.summary`: counts, gaps by code, target
  results, and the cancellation flag.

### 3.4 Web UI

* **Stack.** React and TypeScript with no component library; the CSS is
  hand-written for density (13 px, compact tables, sticky headers).
* **Navigation state** lives in the URL hash, so reloads and back/forward
  work.
* **Polling.** `useApi` refetches when its inputs change or when
  `data_version` changes while no scan is running.
* **Mouse-driven.**
  * Click selects a row for the detail pane; double-click opens a
    directory.
  * Checkboxes select rows, and shift-click selects a range.
  * A selection bar offers Inventory, Verify, Check, Tag, and Add to
    Lineage.
  * Status chips filter the table; column headers sort it.
  * Tag and Lineage actions are disabled with an explanation unless every
    selected file is verified.
* **Busy state.** A progress bar with a Cancel button while a scan runs.
  Detail requests made during a scan show "waiting for scan #N to release
  the catalog" instead of failing.

### 3.5 Testing approach

* **Real filesystem.** Integration tests run real scans over trees created
  under `.local/test-tmp` (btrfs during development).
  `tests/common/mod.rs` provides the fixture.
* **Rebuild equivalence.** Most scenario tests end with
  `assert_rebuild_matches()`.
* **Regressions.** `tests/regressions.rs` holds the scenarios an
  independent review found: a sample hiding a mid-read failure, root
  replacement mid-scan, timestamp overflow, target normalization, the
  comparison target after replacement, and immutability holes.
* **Known simplifications.** `tests/limitations.rs` pins down the
  simplification described in 1.2.
* **UI.** The UI was exercised end to end with a headless browser during
  development: inventory, verify, offline changes, Check, acceptance of
  suggestions, Lineage review, tagging, and root adoption. Those scripts are
  scratch and not part of the repository.

---

## 4. Defaults and limits

| Setting | Value | Where |
| --- | --- | --- |
| Sampling policy | 16 blocks × 64 KiB; full read up to 1 MiB | `sample.rs` |
| Retries | 1 after an unstable read or listing; none after an open failure | `scan::RETRY_LIMIT` |
| Write batching | 250 ms; committed before reads of ≥ 8 MiB | `scan::BATCH_INTERVAL`, `LARGE_READ` |
| Read buffer | 1 MiB | `scan::READ_BUFFER` |
| `sniff` head | first 8 KiB | `extract.rs` |
| Verification horizon | 730 days (setting) | `catalog::DEFAULT_VERIFY_HORIZON_DAYS` |
| Directory view | 20,000 entries | `query::ENTRY_LIMIT` |
| API lists | 2,000 Blobs, 500 scans, 500 search hits, 500 history entries | `api.rs` |
| Suggestion evidence | 200 items per suggestion (total count reported) | `advise::EVIDENCE_LIMIT` |
| Lineage proposals | 50 successor, 100 name-sequence; sequences of 2–20 | `advise.rs` |
| Shutdown grace | 30 s for a cancelled scan to finalize | `filestore-server/src/main.rs` |
| Listen address | `127.0.0.1:7878` | CLI flag `--listen` |

## 5. Measured performance

Release build, btrfs on NVMe, files in the page cache unless noted.

| Workload | Result |
| --- | --- |
| Test corpus (2,052 entries, 4.4 GiB): Inventory | 0.17 s |
| … Verify | 6.5 s (about 700 MB/s; BLAKE3 single-threaded) |
| … Check (1,265 full reads ≤ 1 MiB, 777 samples) | 1.6 s |
| … `filestore rebuild` | 0.4 s |
| Synthetic 100,000 small files in 2,000 directories: Inventory | 7 s |
| … Verify | 32 s (about 3,100 files/s, dominated by per-file overhead) |
| … Check | 22 s |
| … rebuild of three scans | 13 s |
| API on the 100k catalog | directory views ~3 ms, tree 120 ms (first, uncached), suggestions ~1 s |

Hashing is single-threaded. A cold disk or a spinning disk sets the pace for
Verify; Check's fixed 16 seeks per large file are its dominant cost on HDDs.
