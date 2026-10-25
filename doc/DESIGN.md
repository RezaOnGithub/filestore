# Filestore: File Evidence, Presence Checks, and Semantic Lineages

## 1. Status and Intent

This is the current provisional design and implementation planning baseline. The
tables below are conceptual, not final SQLite DDL. Earlier planning documents,
including the retired scanner notes, remain in Git history.
[Design questions and decisions](OPEN_QUESTIONS.md) records the decisions in Q1
through Q14 and the questions that remain open; section 15 lists the remaining
implementation work. [SCHEMA.dot](SCHEMA.dot) draws the tables of section 13 and
is rendered with `dot -Tsvg`. [IDEAS.md](IDEAS.md) collects proposals that are
not yet part of this design.

Filestore catalogs files in an ordinary filesystem tree. Users can edit, rename,
copy, remove, and restore files without going through Filestore. Scanning
records what can be established afterward. The application retains exact-content
identities, observation history, classifications, and user-defined revision
relationships even when the corresponding files disappear.

The prototype targets a slowly changing personal archive. Rust and SQLite are
the implementation baseline. The filesystem owns placement and bytes; the
catalog owns evidence, derived metadata, and semantic records. Filestore does
not store the managed files, recover deleted bytes, or intercept filesystem
operations. The catalog does hold data that extractors derive from file
contents, including manifest records, which copy what a manifest file says.

The goal is to reduce content reads while preserving exact Blob identities and
semantic records. Continuous file tracking is not required. An uncertain
location can be inventoried again and verified when the user needs its identity;
its uncertainty does not rewrite an existing Blob, tag, or revision.

The central user workflow is:

```text
scan a scope
    -> inspect presence, coverage, and uncertainty
    -> receive suggested actions with supporting evidence
    -> verify selected files or accept a semantic change
    -> retain both the original observations and the user's decision
```

History growth is accepted. Preserve repeated observations, failed attempts,
scan results, and semantic change history. Do not weaken evidence or introduce
automatic history pruning to reduce database size.

## 2. Terms and Boundaries

| Term                  | Meaning                                                                                                                                                                                                                                                         |
| --------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| File                  | In this document, only the object on disk that a path names and a scan opens. The catalog has no File record and no File ID.                                                                                                                                    |
| Blob                  | One exact byte sequence, recognized by a full-content hash under the chosen cryptographic assumptions.                                                                                                                                                          |
| Blob ID               | A stable random public identifier for a Blob, independent of its paths and content fingerprints.                                                                                                                                                                |
| Content digest        | The BLAKE3 hash of a Blob's bytes, used to recognize exact content. Private to the catalog.                                                                                                                                                                     |
| Location              | A sequence of native filename components relative to a configured root; a namespace slot, not the file occupying it.                                                                                                                                            |
| Observation           | An immutable record of one sighting or measurement, with its source and time. Later measurements do not fill in or overwrite it.                                                                                                                                |
| Occurrence            | A derived grouping of file observations at one Location with compatible continuity, with its content evidence: verified as a Blob, plausibly a Blob, ruled out as a Blob, or unidentified. Not a stored identity; the file counterpart of a directory instance. |
| Trigger observation   | An earlier observation in the same scan that prompted a lookup or read. It is provenance for the work, not the owner of newly measured properties.                                                                                                              |
| Opened observation    | A fresh observation of the object actually opened, recorded from its handle before reading a file or listing a directory.                                                                                                                                       |
| Native identity       | Filesystem and inode information, with optional generation information; evidence about a local filesystem object.                                                                                                                                               |
| Root binding          | A recorded association between the catalog's root and a physical directory, initially a candidate until activated.                                                                                                                                              |
| Identity epoch        | A period during which native identifiers may be compared within a recorded filesystem context.                                                                                                                                                                  |
| Directory instance    | A derived grouping of directory observations with compatible continuity; not an eternal directory identity.                                                                                                                                                     |
| Enumeration           | One attempt to list a directory's immediate entries. A complete enumeration says nothing by itself about child contents.                                                                                                                                        |
| Scan scope            | The requested files or directories, depth, and traversal boundaries.                                                                                                                                                                                            |
| Scan operation        | `inventory`, `verify`, or `check`, independent of scope and run outcome.                                                                                                                                                                                        |
| Inventory             | Observe directory entries and filesystem metadata without reading file contents.                                                                                                                                                                                |
| Verify                | Read and hash all bytes of each requested regular file, recognize its Blob, and check its location.                                                                                                                                                             |
| Check                 | Compare a sampled fingerprint with a known Blob baseline; use Verify when the rounded sampling plan covers the whole file. Formerly called a quick scan.                                                                                                        |
| Run outcome           | Whether the requested work succeeded, finished with gaps, failed, or was abandoned. It does not describe the size of the scope or prove exact content identity.                                                                                                 |
| Content recognition   | Identification of exact bytes by an accepted full read, independent of their current location.                                                                                                                                                                  |
| Location verification | Content recognition plus confirmation that the observed location still denotes the read object through compatible ancestors.                                                                                                                                    |
| Sample baseline       | A fingerprint of selected blocks computed during the accepted full read that recognized a Blob. Private to the catalog.                                                                                                                                         |
| Lineage               | A user-defined semantic identity containing a revision graph; formerly called Artifact.                                                                                                                                                                         |
| Revision              | A node in one Lineage, referring to one exact Blob.                                                                                                                                                                                                             |
| Derived metadata      | An extractor's output attached to a Blob, such as a detected format, a perceptual fingerprint, or a thumbnail. Distinct from filesystem metadata, which belongs to observations.                                                                                |
| Extractor             | A program that computes data from the bytes of a verified Blob, as a function of those bytes alone. It identifies itself by name, version, and configuration, and it suggests nothing. Formerly part of "analysis".                                             |
| Extractor run         | One extractor identity executing within one scan. Every extractor output records its run.                                                                                                                                                                       |
| Manifest              | A Blob whose content an extractor has rendered as JSON records that passed a registered schema.                                                                                                                                                                 |
| Link statement        | A submitted assertion that a manifest describes a set of Blobs, each named by Blob ID or by a verified path.                                                                                                                                                    |
| Bundle                | A link statement that also fixes each file's relative path: a declared identity for a group of files whose layout must not change.                                                                                                                              |
| Advisor               | A heuristic that reads the catalog and proposes suggestions for day-to-day operation. It reads no file content. Formerly called analyzer.                                                                                                                       |
| Suggestion            | A proposed user action, accompanied by evidence, uncertainty, and preconditions.                                                                                                                                                                                |

Four nouns carry the model:

- A **Blob** is which bytes. When someone says "this file" and means its
  content, they mean a Blob. Tags, revisions, and derived metadata attach here.
- A **Location** is where: a path under the root, whatever currently sits there.
- An **Observation** is what one scan saw at a Location at one time: a name, a
  native identity, a size, timestamps. It is evidence and never an identity.
- An **Occurrence** is a Blob at a Location as far as the evidence shows. It is
  computed from observations and reads by the rules in section 7.3, and it
  carries the strength of that evidence.

"File" in this document means only the object on disk. The catalog has no File
record. A `filesystem_observations` row holds a name, a native identity, and
metadata, but it is one sighting, not a file: an unchanged file listed by five
scans in one identity epoch has five observations and one occurrence. A Blob can
have many occurrences or none, and an unidentified occurrence has no Blob. Facts
about content belong to the Blob, facts about placement belong to observations,
and meaning that should survive edits and renames belongs to a Lineage.

There is deliberately no durable, scanner-assigned identity for a file or
directory across its entire lifetime. Observations have public IDs so they can
be referenced, but two observations with different IDs may describe the same
native object. Giving an observation an ID does not establish continuity.

The dependency direction is:

```text
scanner facts -> shared evidence interpretation -> advisors and frontends
      |                       |                            |
      |                 qualified views             suggested actions
      |                                                    |
      +--------------------> user operations <-------------+
```

Plumbing records observations, reads, coverage, Blobs, and baselines. It also
provides a shared evidence interpretation component: the rules that compute
qualified directory continuity, coverage, and Blob presence. Advisors and
frontends consume the same results rather than each reconstructing those rules.
Advisors suggest what the user could do next. Only a user operation establishes
Lineage membership, revision ancestry, tag changes, links between a manifest and
the Blobs it describes, or a bundle. None of those semantic decisions rewrites
scanner facts.

Use the operation names **Inventory**, **Verify**, and **Check** in commands and
suggestions: "Inventory this directory", "Verify these three files", or "Check
this archive". A scan is one execution of an operation over a requested scope.
Its name describes the requested work, not its outcome. Reserve "full read" for
reading all bytes of an individual file. Avoid "full scan" and "partial scan":
both blur operation, scope, and completion. A limited scope is intentional;
unfinished requested work is a run that finished with gaps.

Use "content recognition" and "location verification" when their distinction
matters. "Verify this file" means request location verification. A "verified
Blob at this location" requires both accepted content and location checks;
merely recognizing bytes from an open handle is insufficient.

### 2.1 Evidence interpretation contract

An interpretation is a derived result, not a new scanner observation. Each query
identifies its root binding, identity epoch, input publication boundary, and
interpretation version. Results carry their scope, observation intervals, typed
source references, assumptions, and structured reason codes. Any additional
inputs that affect interpretation, such as the selected as-of time, are
explicit. The same facts, parameters, and rule version must produce the same
result.

The component exposes separate answers for object continuity, location
attachment, namespace coverage, descendant traversal, and content evidence. It
never replaces them with one generic confidence score. For continuity and
attachment, it distinguishes `compatible`, `contradicted`, and `unresolved` as
defined in section 7. Coverage queries also identify the work that prevents a
stronger conclusion, as defined in section 8.

Porcelain can organize historical information for convenient browsing, but it
must preserve these distinctions. For example, it may show the previous tree
while scanning a restored root without representing that tree as established
contents of the restored directories. The plumbing supplies the qualified
relationships and their explanations; the frontend chooses their presentation.

## 3. Filesystem Baseline: Locations Persist, Inodes Are Evidence

### 3.1 Chosen platform and layout

The prototype supports one configured root on a local Linux filesystem with
case-sensitive, byte-preserving names. It records regular files, directories,
symlinks, and other entry types, but reads content only from regular files.
Symlinks are never followed. Nested mounts, including bind mounts, are traversal
boundaries. Their entries are reported as boundaries, not empty directories.
Network filesystems and alternative filename-equality rules are outside the
initial supported baseline.

Place private catalog state in a sibling directory outside the managed tree:

```text
collection/
    catalog/        database and application-private state
    files/          configured scan root
```

Store the root location relative to the catalog directory. Moving `collection`
together preserves this relationship. Reject a configuration with catalog state
inside the managed root. Backups of catalog state must also be kept outside the
managed tree. These choices avoid scanning the application's changing database,
WAL, temporary files, or backups as archive content.

Names are stored losslessly as byte strings. A root-relative path is encoded as
an unambiguous sequence of components. Do not apply Unicode normalization or
case folding to filesystem names. Display escaping is separate from storage.

### 3.2 Why neither paths nor inodes are sufficient alone

Persist the native name and parent observation, and record device/inode identity
alongside them. A location can be reconstructed for every observation. Native
identity is never the primary key of a durable file, directory, or Blob.

Within a known filesystem context, matching native identity provides stronger
continuity evidence than matching paths alone: a new object can replace an old
one at the same path, while a rename can preserve the native object. However,
inodes can be reused, and copying or restoring a backup need not preserve them.
A matching inode is therefore a local clue, not permanent identity.

Hard links produce separate location observations with the same native identity
and, when verified, the same Blob. Native identity is not unique across
observations. The first prototype verifies each requested occurrence rather than
transferring a successful read to another path solely through an inode match.

The chosen recovery baseline is full content recognition. A restored file can
recover its existing Blob ID and Blob tags regardless of its new inode or path,
provided the catalog was preserved. Directory history may remain disconnected
after restoration. This is preferable to inventing filesystem continuity in
order to preserve semantic content identity.

### 3.3 Bindings, identity epochs, and restores

Keep a stable ID for the archive. Root bindings describe configured directories
and the native root identities observed there. A candidate binding can be
recorded and scanned without becoming the active binding. An append-only
activation record establishes which binding the user has adopted and preserves
the reason and any reviewed comparison evidence. Moving the catalog and managed
tree together does not itself require a different binding when their relative
layout and comparable native identities remain valid.

Record the filesystem comparison context separately from the root object being
checked. The context is the persistent identity of the filesystem holding the
root: its filesystem UUID and type, with a subvolume identifier where the
filesystem has them. It excludes the boot instance, mount namespace, mount ID,
and device numbers, which change across reboots and remounts of an ordinary PC
while the filesystem stays the same. First determine whether that context is
comparable; if it is, compare the observed root with the expected root before
changing identity epochs. A differing root in a comparable context is
replacement evidence. An incomparable context makes native continuity
unresolved, even if numeric inode values agree. Do not let the changed root
identity itself disable an otherwise valid comparison.

Each scan records an identity epoch within its binding. Reuse an epoch across
scanner restarts, remounts, and reboots while the comparison context can be
checked and still agrees. Start a new epoch for a new candidate binding, after a
changed or uncheckable context, or after an explicit restore reset. Adoption
retains the candidate epoch if its context still agrees. Native continuity is
considered only within an epoch; a boundary does not establish replacement,
deletion, or content change. Old observations retain their meaning as history.

A reboot or remount of the same filesystem keeps the epoch, because inode
numbers on a persistent local filesystem survive it. Occurrences keep their
verification ages and directories keep their listing coverage. Within an epoch,
compare native identity by inode number and generation. Record the device
number, but compare it only within one scan to detect traversal boundaries; it
can differ between mounts of the same filesystem. A filesystem that cannot
report a stable identity is outside the supported baseline.

When the configured directory has been replaced or its context cannot be
identified as the adopted one, offer Check against a chosen catalog reference
view. The user can adopt the new filesystem identities after reviewing that
comparison. This supports restoring both the catalog and archive on another host
without requiring the old inode numbers to survive. A missing configured root
remains a scan failure; it is not an empty candidate tree.

#### Comparing a candidate context

Freeze the reference view by its binding, epoch, publication boundary,
interpretation version, and scope. Preserve the reference's observation ages and
coverage limitations. Compare the candidate by root-relative paths, entry types,
and content evidence. Inode differences are expected and are not discrepancies.
The comparison scan uses a candidate binding and fresh native observations; it
never joins old and new directory instances merely because their paths agree.
Store the frozen comparison reference on the scan request. Baseline lookup for
that request uses the specified reference view rather than the candidate's
initially empty history, and each comparison retains its source read or
baseline.

Run Check over the candidate scope using section 6.3's byte budget. Compare
against the reference Blob's size and compatible baseline, and use Verify when
the rounded plan covers the whole file. A sampled file with no compatible
baseline stays unresolved; the user can request Verify on selected files before
deciding.

Classify every expected regular-file location exactly once:

| Outcome        | Meaning                                                                                                                                   |
| -------------- | ----------------------------------------------------------------------------------------------------------------------------------------- |
| Verified match | Accepted verification recognizes the expected Blob at that location.                                                                      |
| Sample match   | Size and compatible sampled content agree, with a confirmed location check.                                                               |
| Changed        | Accepted size or content evidence, or a known entry-type difference, contradicts the expected file.                                       |
| Missing        | Usable namespace coverage establishes that the expected location is absent.                                                               |
| Unresolved     | The reference lacks comparable content evidence, a baseline is unavailable, access or stability checks fail, or coverage is insufficient. |

Count candidate regular-file locations outside the expected-file set as added.
An added file is new relative to the reference view; this does not establish
when it was created. Do not count a changed expected-file slot as both changed
and added. Report directory and other entry-type differences separately,
including empty directories. Expected files under an inaccessible or unvisited
subtree are unresolved, not missing. Full and sample matches remain separate
counts.

The comparison report uses this exception budget:

```text
exceptions = changed + missing + unresolved + added
compared_locations = expected_file_locations + added_file_locations
exception_rate = exceptions / compared_locations
```

The initial low-discrepancy recommendation requires complete usable namespace
coverage of the candidate root under the stated boundary policy, an exception
rate of at most 1%, and at most 20 exceptions. A partial reference view must be
identified as such and cannot support a whole-root low-discrepancy
recommendation for unrecorded areas. With no useful expected files, or a zero
denominator, the score is insufficient evidence rather than perfect agreement.
Partial candidate coverage similarly prevents the low-discrepancy
recommendation.

The thresholds are configurable recommendation defaults, not statistical
confidence or permission to claim equality. Include unresolved files in the
budget so failure to check content cannot inflate agreement. Sample matches
count as agreement under this policy, with their ordinary blind spots intact.
The interface may still offer explicit adoption when the thresholds are exceeded
or scope remains unresolved, with the limitations presented for review.

Show the exceptions by top-level directory, the affected expected byte counts,
and the number of distinct matched Blobs, keeping fully recognized and
sample-supported results distinguishable. An entirely missing top-level subtree
or a significant entry-type change requires explicit review even when aggregate
file counts are small. New files and empty-directory differences must remain
visible; the scalar score does not summarize every structural difference.

For example, 10,000 expected files might yield 2,500 verified matches, 7,488
sample matches, 3 changed, 4 missing, and 5 unresolved files, plus 6 added
files. That is 18 exceptions among 10,006 locations, approximately 0.18%. With
complete coverage and structural differences reviewed, it qualifies for the
initial adoption recommendation. It is not a statement that 99.82% of bytes are
identical.

#### Adopting the new identities

Candidate scan results can be published for review within the candidate binding
without changing the active archive view. On acceptance, append an activation
record for that binding, retain its fresh identity epoch and native
observations, and record the reference view, comparison evidence, thresholds,
exceptions, and limitations the user reviewed. The activation and its provenance
are one transaction. Adoption does not rewrite old identities or assert
equivalence between old and new inodes.

Existing candidate evidence keeps its original scope, time, and strength when
the binding becomes active. Fresh complete listings provide namespace coverage;
sample matches remain sample matches; unresolved locations stay unresolved. Old
location-verification times and unscanned directory coverage are not refreshed.
Full content recognition can recover existing Blob IDs and tags regardless of
changed paths or native identifiers.

Before activation, ensure that the expected previous active binding has not
changed and recheck the candidate root and comparison context. If they changed,
require a fresh comparison or review of a new candidate. Later published work
that changes the report must also be reviewed. These checks do not create a
filesystem snapshot or detect every change made after scanning; retain the
comparison interval and age in the accepted record.

An in-place restore may be indistinguishable from other offline changes. Provide
an explicit restore action that starts a candidate comparison even if the
apparent root identity is unchanged. Without that signal, the scanner cannot
promise to detect restoration that preserves all inspected metadata.

Backing up the catalog means taking a consistent database backup, including its
semantic history. A live database file must not simply be copied while ignoring
journal state. A files-only backup cannot reconstruct random public IDs or user
metadata after the catalog is lost.

### 3.4 Open handles and scan stability

Resolve targets from an open root directory handle. Restrict every component to
the root, reject symlink traversal, and reject mount crossings. On the supported
Linux baseline, use `openat2` resolution constraints or an implementation with
equivalent checked behavior; do not silently fall back to unrestricted path
opens. `O_NOFOLLOW` alone only protects the final component, while the
resolution flags constrain the whole path. See the
[Linux `openat2` documentation](https://man7.org/linux/man-pages/man2/openat2.2.html).

For a content read:

1. Open the regular file, compare it with the trigger observation if present,
   and create a fresh opened observation from the handle's identity, size, and
   change metadata. Resolve changed or uncertain ancestor anchors as described
   in section 3.5. The read refers to this opened observation.
2. Read the required bytes from that handle, recording actual byte counts.
   During a full read, feed the same bytes to the streaming extractors requested
   for this file.
3. After a complete full read, run the requested seeking extractors on the same
   open handle.
4. Read handle metadata again and compare the relevant fields.
5. Re-resolve the location under the root and check that its components and
   final entry still denote the observed objects.
6. Record the content and location outcomes independently. A complete full read
   with acceptable handle stability can recognize a Blob even when the final
   location check fails. Discard computed content tokens and extractor outputs
   if the content read itself was incomplete or unstable.

Steps 2 and 3 are the two points where extractors attach to a read. Section 4.2
defines them. The handle comparison in step 4 covers the time the seeking
extractors spent reading.

A full read requires the expected number of bytes and EOF at the expected end.
Sampling requires all selected ranges to be read completely. A successful read
of an object that has moved away from the observed location does not verify
presence at that location in this prototype. It can still admit the recognized
Blob and its baseline. A location check is `confirmed` when the entry and its
anchor chain pass, `contradicted` when a successfully checked name or ancestor
no longer denotes the observed object, and `unresolved` when access or
comparison failure prevents a decision. Retain the failed component and reason.
An unattempted location check is recorded as `not_checked` and provides no
location confirmation.

For example, the handle checks and full hash may finish successfully, followed
by a rename before the final location check. Record recognized content and
contradicted location attachment. A failed lookup at this stage does not create
complete parent-directory coverage or establish where the file went. Only
accepted full recognition together with a confirmed location is verification at
that location. Record the content-read interval and location-check time
separately; even their agreement remains subject to the concurrency limitations
below.

These checks detect ordinary concurrent mutation; they are not snapshot
isolation. Even one `stat` result can contain fields observed at different
moments. See the
[Linux `stat` documentation](https://man7.org/linux/man-pages/man2/stat.2.html).
A rename can also move an already-open object after path resolution. If
detected, its affected location evidence is unusable. The baseline assumes a
non-adversarial archive and does not promise that every byte read remained under
the root throughout concurrent renames.

### 3.5 Attributing measurements to the opened object

An observation records what was seen at its own time. No later read may
overwrite it, fill in its unknown fields with another object's measurements, or
backdate new information into it. A trigger observation explains why work was
attempted; it is not the owner of the resulting content or metadata.

For every successful file open used by a read attempt, create a fresh
observation with role `opened_handle`. Capture its metadata directly from the
handle before reading. This simple rule also applies when the native identity
agrees with the trigger: the earlier sighting and the later measurements remain
separate. Use explicit attempt links to retain their relationship, and interpret
native continuity from the recorded evidence.

Compare the trigger and opened identities only within a comparable context:

- If they differ, the opened object is a new sighting at the location. Preserve
  both observations and resolve the affected ancestor chain again. Do not move
  size, timestamps, Blob identity, sample results, or any other properties from
  the opened object onto the trigger observation.
- If they agree, continuity is compatible under the usual inode-reuse
  assumptions. An in-place edit can still have changed the metadata or content.
  Use the fresh handle measurements for the read and retain the old observation
  unchanged; matching native identity does not verify content.
- If comparison is unavailable, keep continuity unresolved. The fresh handle
  observation may still support content recognition. Location verification
  requires its own confirmed path and ancestor checks.

For example:

```text
O1: enumeration sees /report.pdf, inode 100, metadata measured at t1
O2: later open sees /report.pdf, inode 200, metadata measured at t2
R1: trigger = O1, opened observation = O2
    recognized Blob and read/location outcomes belong to O2 and R1
```

O2 has no membership in O1's enumeration attempt. Its name and scope can come
from the requested lookup, but its native identity and measured properties come
from the opened handle. O1 stays an immutable historical sighting, including any
unknown fields. O2 is separate positive evidence at its own time when its
location checks pass; it does not retroactively change the completed listing.

An open failure yields no opened observation and no content measurements. Retain
the trigger, failure, and diagnostic in the attempt, without inventing an object
at the requested location. If a selected target cannot even be observed, record
that failure in the target result and diagnostics. Insert a file-read attempt
after open resolution, with its opened reference already fixed or explicitly
absent; do not patch an immutable attempt later to refer to a different object.

Apply the same ownership rule when opening a directory for enumeration: its
entries and attempt metadata belong to the freshly opened directory observation,
with the earlier trigger retained separately. An open failure cannot produce a
complete listing. Replacement of an ancestor requires fresh anchors; reuse of a
path string alone cannot attach the new object's evidence to the old ancestor.

The scanner can retain a successful read of inode 200 once these checks pass;
replacement before opening is not itself a reason to discard valid bytes or
rescan unrelated siblings. Replacement during reading or a failed final location
check is still handled by the independent content and location outcomes in
section 3.4. Selection among the resulting observations is a separate projection
question; none of those selection rules may transfer properties between them.

## 4. Exact Content Identity and Privacy

For an accepted full read of bytes `F`:

```text
content_digest = BLAKE3(F)        # 32 bytes; private to the catalog
blob_id = random 256-bit value    # public; assigned when admitting new content
```

Store the content digest, public Blob ID, exact size, and scheme version. A
unique content digest is the final arbiter of duplicate admission inside a write
transaction. A size inconsistency on a digest match is an integrity error, not a
second Blob.

Exact duplicate content shares one Blob and its Blob-level tags. A Blob survives
even when no location currently verifies or plausibly contains it. Public Blob
IDs remain stable; they matter as durable references used by revisions, tags,
and external consumers. They are identifiers, not authorization credentials.

The digest scheme and its encodings are versioned. Changing the scheme requires
an explicit migration preserving public Blob IDs. Migration rereads files, so a
Blob with no available occurrence cannot be migrated. Keep the old recognition
scheme available until a migration can account for every retained Blob.

### 4.1 Privacy boundary

Two rules define the boundary:

1. **The catalog is as sensitive as the managed files.** It holds paths, sizes,
   timestamps, content digests, sample tokens, tags, history, and derived
   metadata such as thumbnails or perceptual fingerprints. Anyone holding it can
   test candidate content for membership and read everything recorded about the
   archive. Nothing in this design makes a leaked catalog safe. The database,
   its journals and backups, stored extractor outputs, and diagnostics all sit
   inside the boundary and need the same protection as the files.
2. **Outside the catalog, content is named only by Blob ID.** A Blob ID is
   random: it reveals nothing about the bytes, cannot be matched against a hash
   list, and means nothing in another catalog. Extractors, advisors, frontends,
   and external pipelines refer to content by Blob ID. Content digests and
   sample tokens never appear in command output, logs, diagnostics, exported
   references, URLs, or filenames. The Blob ID is the only name for content that
   a user or another program sees.

Random Blob IDs exist to keep content identity out of other pipelines. They do
not encrypt the catalog or the managed files. Content digests are deliberately
not keyed with a database secret: such a secret would be stored beside the
digests it protects, and rule 2 already keeps digests inside the catalog.

### 4.2 Derived metadata and extractors

Derived metadata describes a Blob's content: a detected format, a perceptual
fingerprint, a thumbnail. An extractor computes it from the Blob's bytes, and
the result attaches to the Blob, so every occurrence of the same content shares
it. It is distinct from filesystem metadata such as size, timestamps, and native
identity, which belongs to observations.

**A function of the bytes.** An extractor's output depends only on the bytes it
read and on its own identity. It must not depend on the file's name, its
Location, its neighbors, or the time. The same extractor therefore produces the
same result for a Blob wherever and whenever it runs, and a result computed from
a file in one directory holds for identical bytes found anywhere else. Anything
that depends on context, such as which other files a manifest describes, is not
derived metadata. A link statement states it, under section 12.2.

**Identity and runs.** An extractor identifies itself by name, version, and
configuration. The configuration is a canonical encoding of every setting that
affects the output. A run is one extractor identity executing within one scan.
Every output records its run and the accepted full read that supplied its bytes,
so a result can be traced to the program, version, settings, scan, and read that
produced it.

**Input.** An extractor must establish that its input bytes belong to the Blob
it reports on. It reads through a handle whose full read recognized that Blob
and passed the stability checks of section 3.4. A historical path association or
a sample match is not enough. A confirmed location is unnecessary, because the
result describes the bytes and not where they were found.

**One result per Blob and identity.** For one Blob and one extractor identity
the catalog keeps one result. A later run with the same identity that reproduces
it adds nothing. A later run that produces something different is an
`extractor_output_conflict`: keep the existing result, record a diagnostic
naming both runs, and admit nothing from the conflicting run for that Blob. A
different version, configuration, or extractor adds its own result alongside.
Nothing is overwritten.

**Corrections.** A faulty extractor is corrected by running a fixed version,
which adds new results. For one Blob, extractor name, and configuration, default
views show the most recently created result, and earlier ones stay available. A
user operation can retract a run. Its results remain stored but leave default
views.

**Outputs.** An extractor produces derived metadata, or it renders a manifest's
content as records under section 12.1. Neither contributes to Blob identity,
directory continuity, or occurrence evidence. A similarity result can motivate
an advisor's suggestion; it never establishes that two files hold the same Blob.

**Execution.** Extractors run only during the full reads of a Verify request
that names them. Check runs none, including when its plan covers the whole file.
An extractor attaches to the read of section 3.4 in one of two ways:

- A streaming extractor consumes bytes in order, as the content hasher and the
  sample token do. The read loop feeds it the bytes it reads. Type sniffing and
  rendering a JSON manifest are of this kind.
- A seeking extractor needs a file descriptor, as a thumbnailer or a format
  converter does. It runs after the hash completes, on the same open handle,
  before the final handle comparison. Its reads normally come from the page
  cache. The Blob is known by then, so the extractor is skipped when a result
  for its identity already exists.

Outputs stay provisional until the read is accepted. They are committed in the
same transaction as the content result and, like an admitted Blob under section
9.3, do not depend on the scan's publication. A failed or timed-out extractor
leaves the content result intact, produces no output, and records a diagnostic.

**Roles.** Extractors and advisors are separate. An extractor reads file content
and produces data about one Blob; it suggests nothing. An advisor reads the
catalog and proposes actions; it reads no file content. Only extractors and
scans access the managed tree.

Storage of large outputs such as thumbnails, and running an extractor outside a
scan to backfill existing Blobs, are not yet designed. [IDEAS.md](IDEAS.md)
holds the current proposals.

## 5. What a Scan Requests and What It Proves

### 5.1 Operation and scope are independent

A scan request specifies **Inventory**, **Verify**, or **Check**, plus a list of
root-relative targets. Each target is either:

- a directory with a maximum descendant-directory depth, or unlimited depth;
- an individual file whose metadata or content is to be inspected.

Directory depth zero means list the target's immediate entries and apply the
operation to its immediate regular files, without enumerating child directories.
Increasing depth permits that many child-directory levels. Normalize duplicate
and overlapping targets before execution. Retain the normalized request,
boundary rules, sampling parameters, retry limits, and scanner version.

A Verify request may also name extractors under section 4.2: a set applied to
every file the request reads in full, and extractors for individual file
targets, such as the one that renders a manifest. The request records the
extractor and its configuration; the session records the version that ran.

| Operation   | Work performed                                                                                                                                                                                   | Content conclusion                                                                         |
| ----------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------ |
| `inventory` | Enumerate requested directories and observe types, native identities, sizes, and timestamps. Do not read file contents.                                                                          | No new exact Blob identity or sample match. Metadata differences can prompt further work.  |
| `verify`    | Perform the same metadata work, read all bytes of each requested regular file, recognize its Blob, establish a sample baseline by default, run the requested extractors, and check its location. | Exact recognition; verified occurrence when location checks also pass.                     |
| `check`     | Perform the same metadata work. Compare size and sampled content against a prior Blob using the versioned byte budget. Use Verify when the rounded plan covers the whole file.                   | Verified full-coverage reads; otherwise sample agreement, mismatch, or unresolved content. |

All three operations accept a file, selection, directory, or root scope. Verify
on three files can succeed without checking siblings. Inventory of the root can
refresh the observed hierarchy without reading content. Check of the root can
succeed while many files have only sample agreement. Operation names determine
neither scope nor completeness. Inventory's cost principally follows the number
of entries; it still records inaccessible directories and traversal boundaries.

A request becomes durable while database access is available. Another command
must wait if a scan owns the database; merely waiting or queuing externally
creates no scanner evidence. A separate session records execution and the actual
policy used. Previously persisted requests can wait for execution under section
9.1's exclusive-access rule.

Run outcomes are `succeeded`, `finished_with_gaps`, `failed`, and `abandoned`.
They describe execution of requested work, as defined in section 9.2. Listing
coverage and content evidence remain separate. For example:

```text
Operation: verify
Scope: b.mp4, c.zip
Run outcome: succeeded
Evidence: both selected locations verified
Parent listings and siblings: not refreshed by this request
```

Inventory records content work as `not_requested`. This is not a failed content
check and does not erase a prior verification. It also does not renew that
verification's date. Metadata that contradicts a previous attachment or content
expectation is handled by the transition rules in section 7.3.

### 5.2 Anchors are observations, not enumerations

A selected subtree or file must be anchored to the root through freshly observed
path components. Record these ancestor observations in the same scan, with a
role distinguishing path resolution from directory-list membership.

Opening `files/a/b/report.pdf` observes the necessary ancestors. It does not
mean that `a` or `b` was listed. Consequently, verifying `report.pdf` can
establish its observed presence without refreshing the sibling list or absence
knowledge of either parent.

A failed selected-file lookup is retained with its failed component and reason.
The initial projection does not turn that lookup alone into an authoritative
listing difference. Offer a parent-directory scan to resolve namespace absence.

### 5.3 Three separate coverage dimensions

| Dimension                   | Examples of recorded state                                                                                |
| --------------------------- | --------------------------------------------------------------------------------------------------------- |
| Immediate namespace listing | Complete and apparently stable; incomplete; failed; unstable; not attempted.                              |
| Descendant traversal        | Not requested; partial; complete within requested depth and boundaries.                                   |
| File content evidence       | Verified; sample match; sample mismatch; size mismatch; no baseline; unreadable; unstable; not attempted. |

Completeness is always relative to an explicit scope and interval. A complete
listing may contain an entry whose metadata or content could not be read. Record
that entry by name with unknown fields; its read failure does not erase listing
coverage. Likewise, an inaccessible child directory does not invalidate a usable
listing of its parent.

Boundary exclusions must remain visible. A run that succeeds under the
no-mount-crossing policy is not proof about files inside excluded mounts.
Reaching an operational resource limit records partial work and its cause; it
must not silently shorten the requested scope and then report completion.

## 6. Check: Comparing Expected Blob Content

### 6.1 Purpose

Check mainly answers: **does content previously verified at a location appear
still to be there, and what needs a full read?** It reduces reads when checking
presence in a slowly changing archive. It does not discover exact duplicates or
previously unknown revisions through sampling.

Check uses section 6.3's parameterized read budget, anchored initially at 80% of
a 1 MiB file and growing logarithmically above that size. For a sampled
comparison, select a prior verified Blob and its compatible baseline. The usual
selection is the last verified Blob at that root-relative location; record its
source explicitly. An individual-file request can instead name a comparison
Blob, for example after reviewing a possible move. This selects a baseline to
test; it does not assert that the file contains the Blob. Do not search for
comparison candidates automatically through inode equality.

A previous Blob at the same path is a comparison target even if inode continuity
is uncertain or the root was restored. Testing against it does not assert that
the filesystem object is the same. Don't treat it as plausible when intervening
usable evidence rules it out; retain it as historical comparison evidence. The
baseline scanner does not search all Blobs for sample candidates or infer a new
location's Blob from an inode match alone.

If a comparison Blob exists and stable size evidence differs, record a size
mismatch without reading content. Otherwise calculate the sampling plan. If it
covers the whole file, run Verify, even if no baseline exists. For a partial
plan, require a comparison Blob and compatible baseline before reading its
ranges. Missing or incompatible baselines produce `no_baseline` and a Verify
suggestion; do not read purposeless samples or silently hash the whole file.

### 6.2 Interpretation

The location conclusions below require a confirmed location check for the read
or size comparison. Content outcomes whose locations are contradicted or
unresolved remain evidence about the read object, not about the occurrence at
that path.

| Result                                                           | Supported conclusion                                                           | Suggested next action                              |
| ---------------------------------------------------------------- | ------------------------------------------------------------------------------ | -------------------------------------------------- |
| Full read recognizes A and location is confirmed                 | A was verified at this location under the read and location checks.            | None solely for content identity.                  |
| Sample agrees with A and location is confirmed                   | A plausibly remains; no difference was found in the sampled ranges.            | Verification when certainty is needed.             |
| Stable sample differs from A and location is confirmed           | This observed occurrence differs from A under the stated read assumptions.     | Verify the file to identify its current Blob.      |
| Stable size differs from A and location is confirmed             | This observed occurrence is not A.                                             | Verify the file.                                   |
| No compatible baseline                                           | Current Blob is unresolved.                                                    | Verify the file or selected unresolved files.      |
| Content read fails or is unstable                                | Current content is unknown.                                                    | Resolve access or mutation and retry.              |
| Content is recognized but location is contradicted or unresolved | The read established a Blob; its attachment to this location was not verified. | Resolve or rescan the location and affected scope. |

A mismatch rules out a Blob **at the tested location** only when the location
checks also pass. It never establishes absence elsewhere in the root. A sample
match does not prove presence of that exact Blob. Check can miss file changes
outside sampled ranges and must never present all sample matches as "all
contents verified" or "no changes."

Keep `recognized_blob_id` separate from `compared_blob_id`. Only accepted full
recognition populates the former. Location verification is a derived conclusion
requiring both `full_recognition` and `confirmed` location status. Preserve last
location verification as historical evidence, and show its age alongside the
latest Check result. A later confirmed mismatch suppresses plausible presence
based on an older match. A newer failed or unresolved location check must remain
visible; selecting an older success cannot resolve it.

### 6.3 Sampling policy

The agreed version-1 scheme uses a parameterized byte budget, mandatory first
and last blocks, deterministic uniform sampling of distinct interior blocks, and
one aggregate BLAKE3 token. It is intended to reduce reads when size agrees but
timestamps or filesystem identity leave uncertainty. The initial parameters are
provisional performance defaults; the algorithm and encoding are explicit.

A baseline is created during an accepted full read, using bytes from that same
read. This includes full reads performed by Check when its plan covers the whole
file. Verify creates the configured baseline while streaming by default. An
explicit policy may omit baseline creation; a later sampled Check then needs
another Verify to establish it. Never establish a baseline from a sampled read.

#### Parameters and canonical encoding

| Parameter        | Meaning                                    | Initial value      |
| ---------------- | ------------------------------------------ | ------------------ |
| `v`              | Scheme version                             | `1`                |
| `B`              | Block size in bytes                        | `16384` (16 KiB)   |
| `A`              | Coverage anchor size in bytes              | `1048576` (1 MiB)  |
| `p_num`, `p_den` | Anchor coverage as a reduced fraction      | `4`, `5` (80%)     |
| `G`              | Additional sampled bytes per doubling band | `131072` (128 KiB) |

All sizes are bytes; displayed KiB, MiB, GiB, and TiB use binary units. Require
`B > 0`, `A > 0`, `0 < p_num <= p_den`, and a fraction in lowest terms. `G` may
be zero. Version 1 uses unsigned 64-bit integers and this exact field order:

```text
P = u64_le(v) || u64_le(B) || u64_le(A)
    || u64_le(p_num) || u64_le(p_den) || u64_le(G)
```

`P` is 48 bytes, not a textual serialization or database row ID. The version
fixes the budget calculation, endpoint rules, block selection, hash contexts,
framing, and token length. Changing a parameter creates a distinct policy;
changing an algorithm rule requires a new scheme version. Never reinterpret or
compare a retained baseline under a different policy.

#### Byte budget and full-read fallback

For file size `S`, calculate the integer target `T`:

```text
if S <= A:
    T = ceil_div(p_num * S, p_den)
else:
    d = smallest nonnegative integer such that S <= A * 2**d
    T = ceil_div(p_num * A, p_den) + G * d
T = min(S, T)
```

`ceil_div` means exact integer ceiling division. Use checked, sufficiently wide
arithmetic for products, powers, and division; never use floating-point logs or
allow overflow to change the policy. `S = 0` gives `T = 0`.

There is no separate small-file verification threshold. If rounding the plan to
whole blocks already covers every byte, Check performs the full Verify
procedure, including exact-content recognition and location checks. This also
handles empty files. Otherwise, a compatible prior baseline is required; no
baseline means unresolved content and a suggestion to Verify, not an automatic
full read of a megabyte-scale file. A confirmed size mismatch against the
comparison Blob requires no content read.

With the initial parameters and mandatory endpoints:

| File size | Blocks read | Content bytes read | Coverage     |
| --------- | ----------- | ------------------ | ------------ |
| 64 KiB    | 4           | 64 KiB             | 100%; Verify |
| 256 KiB   | 13          | 208 KiB            | 81.25%       |
| 1 MiB     | 52          | 832 KiB            | 81.25%       |
| 4 MiB     | 68          | 1.0625 MiB         | 26.56%       |
| 64 MiB    | 100         | 1.5625 MiB         | 2.44%        |
| 1 GiB     | 132         | 2.0625 MiB         | 0.201%       |
| 1 TiB     | 212         | 3.3125 MiB         | 0.000316%    |

These counts describe requested content bytes, not physical device I/O or
elapsed time. Benchmark the defaults on intended storage; seek costs and
readahead can reduce the savings. Coverage is not a probability that the whole
file is equal.

#### Block count and selection

Partition the file into blocks of size `B`, with a possibly shorter final block.
Let `N = ceil_div(S, B)`. For `N = 0`, select no blocks. For `N = 1`, select
block zero. For `N >= 2`, always select blocks zero and `N - 1`, then calculate:

```text
M = N - 2
last_length = S - (N - 1) * B
endpoint_bytes = B + last_length
r = min(M, ceil_div(max(0, T - endpoint_bytes), B))
```

Choose exactly `r` of the `M` interior blocks. Accounting for the final block's
actual length ensures the selected bytes reach the budget. If `r = M`, select
all blocks directly and use the full-read fallback; no random draws are needed.

Derive the position stream with BLAKE3's key-derivation mode, used here as a
domain-separated hash; no secret is involved. Restart the stream from byte zero
for every selection calculation:

```text
position_stream = BLAKE3_derive_key_XOF(
    "filestore sample positions v1", P || u64_le(S)
)
```

The context is the exact UTF-8 string shown, with no terminator, and
`P || u64_le(S)` is the key-material input. Read consecutive 8-byte words from
the stream in little-endian order. Obtain an unbiased integer in `[0, m)` by
rejection sampling:

```python
def uniform_below(m):
    limit = 2**64 - (2**64 % m)
    while True:
        x = next_u64_le(position_stream)
        if x < limit:
            return x % m
```

Compute `limit` in a wider type so `2**64` is representable. A rejected word is
consumed. Fix this conversion as part of the protocol; a generic library's
random-range implementation is not the specification.

Use Floyd's sampling algorithm, in increasing `j` order:

```python
chosen = set()
for j in range(M - r, M):
    t = uniform_below(j + 1)
    chosen.add(j if t in chosen else t)
blocks = sorted({0, N - 1} | {x + 1 for x in chosen})
```

The set is used for membership and insertion, never iteration-dependent random
choices. The algorithm chooses a uniform subset of exactly `r` interior indices
without retrying duplicate indices. Sorting fixes the read and fingerprint
order. Use this frozen procedure rather than a generic sampling API that may
switch algorithms. The first and last blocks are mandatory; uniform selection
refers only to the interior subset.

The same policy and size give the same positions in every catalog. Paths, native
identities, timestamps, Blob IDs, and scan IDs are not inputs. Restoring the
tree with its catalog preserves the selection. Equal-sized files share positions
under the same policy. Checks do not rotate or accumulate new sample coverage.

#### Fingerprint framing

Hash the framed sample under a second fixed context:

```text
payload = P || u64_le(S) || u64_le(selected_block_count)
for each selected block in increasing offset order:
    payload += u64_le(byte_offset) || u64_le(actual_length) || block_bytes
sample_token = BLAKE3_derive_key(
    "filestore sample token v1", payload
)                                             # exactly 32 bytes
```

Use the exact context string shown without a terminator. Feed the payload
incrementally into the hasher; no intermediate per-block digest is needed.
Baseline storage contains only the aggregate sample token and its policy, Blob,
and source read references. Check retains its comparison result and baseline
reference; it does not create a baseline from sampled bytes. Never persist
sampled bytes or per-block fingerprints.

Sample tokens are private to the catalog, like content digests, under section
4.1. Block positions are a public function of policy and size, so an edit can be
placed to avoid them; section 3.4's non-adversarial baseline already excludes
that case.

Adjacent selected blocks may be coalesced into one filesystem read, but each
block keeps its separate offset/length/bytes framing. Never add unselected gap
bytes to the fingerprint. Verify feeds selected bytes from its accepted full
read through this same procedure; Check reads only the selected ranges. Require
complete ranges and accepted stability checks. A failed or unstable read yields
no reusable token. Location conclusions additionally require confirmed location
checks, independently of the content comparison.

#### Baseline consistency and implementation validation

The same recognized Blob and canonical policy must produce the same token. If a
later stable full read recognizes an existing Blob but computes a different
token under that policy, treat it as an integrity error. Preserve the old
baseline and record a failed read outcome and diagnostic identifying the Blob
and policy; do not admit the conflicting baseline, silently overwrite it, or
report an ordinary content change. Apply section 9.2's integrity failure rule. A
Check mismatch against a valid baseline is an ordinary content difference, not
this consistency error.

Freeze shared test vectors during implementation for both baseline creation and
Check. Cover empty and tiny files, exact and partial blocks, both sides of the
anchor and doubling boundaries, forced full coverage, high-coverage selection,
Floyd collisions, rejected random words, and large integer bounds. Verify that
coalesced reads preserve the token, different policies cannot compare, and a
changed sampled byte differs while an edit confined to unsampled bytes can pass.
Changing only path, timestamp, or native identity must leave the token
unchanged.

Fixed positions retain blind spots: changes outside them can escape every Check.
Do not attach a numerical confidence claim without a stated change model. There
is no adversarial-tamper guarantee. Full coverage only counts as verification
when the full Verify procedure was actually completed. Missing or incompatible
baselines remain unresolved; candidate searches over baseline-free fingerprints
are outside the prototype.

### 6.4 Verification freshness is a maintenance policy

A combined view has per-entry evidence and ages, not uniform certainty. Expose
when a location was inventoried, when its content was last verified, and the
latest fingerprint comparison and its result. A new Inventory observation does
not advance either content date. Check advances the comparison date; it advances
the verification date only when it actually performs successful verification.

Use a configurable verification horizon, provisionally two years. A freshness
score may improve after a matching Check, but is capped by a ceiling that falls
to zero at that horizon measured from the last eligible location verification.
Repeated matches do not accumulate as independent confirmations or postpone the
deadline. Only verification resets it. The particular score curve is a versioned
maintenance policy; expose the dates and overdue status even without a score.

Zero freshness means verification is overdue, not that the file is absent or
corrupt. A never-verified occurrence is unidentified and has no verification
age; an earlier occurrence's verification cannot supply one. Fingerprint
mismatch, replacement, and access failure remain explicit findings whatever a
score would have been. A later sample match cannot conceal a known content
contradiction. Freshness is not a calibrated probability of byte equality and
never authorizes Blob admission, tagging an unidentified file, or an absence
conclusion.

Compute freshness from retained evidence, an explicit as-of time, and a policy
version. Do not store a decaying score as a scanner fact. An advisor can use the
policy to suggest Verify on selected overdue or unresolved files while leaving
recently checked files alone.

## 7. Directory Identity and Combining Scoped Scans

### 7.1 Directory continuity is a qualified interpretation

No directory has a content hash that identifies its complete evolving subtree. A
pathname can be reused, and an inode can be reused. The interpretation component
therefore computes a provisional directory instance rather than asserting an
eternal directory ID in scanner facts.

Keep three questions separate:

- **Object continuity:** do the observations plausibly describe the same native
  directory object? Comparable matching native identities support this even at
  different paths, under the stated inode-reuse assumptions.
- **Location attachment:** may historical observations be attached to the
  directory represented at this location through this ancestor chain?
- **Listing coverage:** what entries were enumerated, when, and with what
  completeness and stability? Object continuity does not refresh this evidence.

For each proposed continuity or attachment relationship, return one of:

| Outcome        | Meaning                                                      | Example reason codes                                                                                    |
| -------------- | ------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------- |
| `compatible`   | The required evidence agrees under the recorded assumptions. | `native_identity_matches`, `ancestor_attachment_compatible`                                             |
| `contradicted` | Observed evidence conflicts with the proposed relationship.  | `native_identity_differs`, `slot_absent_in_complete_listing`, `entry_type_changed`, `ancestor_replaced` |
| `unresolved`   | Required evidence is missing, incomparable, or insufficient. | `identity_missing`, `identity_epoch_differs`, `ancestor_unresolved`, `location_changed`                 |

Reason codes are versioned, with references to the observations and results that
support them. A native-identity difference is a contradiction only within a
comparable filesystem context. Missing identity or a changed epoch is not a
replacement finding. A relationship result is derived data, not a persisted
scanner assertion or a semantic decision.

Automatic attachment of an older directory listing requires:

- the same root binding and identity epoch;
- the same root-relative location and compatible ancestor attachments;
- matching native filesystem identity, including generation evidence if present;
- no intervening usable observation of absence, replacement, or conflicting
  type.

Missing required evidence leaves attachment unresolved; a known contradiction
prevents attachment. A changed location is not automatically joined to the old
directory instance even when object-continuity evidence is compatible. Such
evidence can support a move suggestion, but the new location needs fresh
enumeration. This avoids transplanting historical descendants through an
uncertain rename or inode-reuse inference. A frontend may separately show the
possible old tree as history without establishing that relationship in the
plumbing.

Matching native identity under these rules is still an assumption of ordinary
filesystem continuity, not proof. Inode reuse that leaves indistinguishable
evidence can escape detection. Re-enumeration and file verification provide
fresh evidence that can support inferences about changes since earlier scans.
They do not generally determine the complete sequence of intervening filesystem
operations.

### 7.2 Directory continuity and listing freshness differ

Even when a directory plausibly continues, its old listing is not refreshed by a
new observation of that directory's name or inode. Compare available directory
change metadata with the metadata saved at the earlier enumeration. A detected
change invalidates reuse of that listing as current coverage, while retaining it
as historical evidence. Matching metadata permits only a labeled last-known
listing under the stated continuity assumptions.

A child-file content edit may leave the containing namespace unchanged. Listing
coverage therefore says nothing by itself about the freshness of child contents.
Every displayed file retains its own content evidence and time.

### 7.3 Projection rules

Build the best-known view by applying a small set of transitions in scan order.
For a regular file, the resulting state at a Location is its occurrence. The
goal is useful inventory with explicit uncertainty. A file does not need a
reconstructed lifetime before it can be checked or verified. Keep references to
the observations behind every derived state; no transition changes a prior
observation or transfers properties to another object.

The ordered inputs and selection rules are:

1. **Choose the context.** Fix the root binding, identity epoch, publication
   boundary, and interpretation version. Candidate bindings have separate views
   until activation. Earlier epochs remain history; equal paths or inode numbers
   do not authorize attachment across an incomparable context.
2. **Process published work in order.** Scan sequence orders runs. Within a run,
   the prototype executes filesystem attempts sequentially, including retries.
   Apply metadata observations at their recorded event order and attempt
   outcomes at their terminal event order. Include all terminal attempts in
   publication's selected input sets, including failures. Target results expose
   requested work left unfinished. Publication never selects only the last
   successful attempt.
3. **Update only the affected dimension and scope.** Keep the latest metadata,
   latest content assessment, last eligible verification, fingerprint comparison
   target and result, and independent location outcome. A verification is
   eligible when it belongs to the same occurrence, meaning the same identity
   epoch and compatible continuity, and no later content evidence contradicts
   it. Keep listing coverage on the directory itself. A newer observation is not
   automatically newer content evidence. Carry each measurement's original time
   even when its terminal result or publication occurs later.
4. **Retain limitations and history.** Apply the table below. A later failure
   leaves earlier success available as last-known evidence with the new failure
   visible. A later successful retry may resolve that failure for the object it
   actually checks; it never combines entries from different enumeration
   attempts. Rebuilds use the same recorded order and produce the same states.

| Finding                                                                               | Resulting state and useful next action                                                                                                                                                                                                                        |
| ------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| A kind of work has never covered this location                                        | That evidence is unavailable. Missing enumeration leaves names unknown; no accepted full read leaves content unidentified. Check requires a known comparison baseline unless it verifies the file in full.                                                    |
| Inventory agrees with previous metadata                                               | Refresh the metadata observation or complete directory listing. Preserve content dates and existing qualifications; agreement does not verify bytes.                                                                                                          |
| Only timestamps, permissions, or other non-content metadata differ                    | Record the new measurements. They do not prove different content; Check can assess the expected Blob. Native-identity differences still affect continuity, as below.                                                                                          |
| A native identity changes, or a restored context is adopted                           | Observe the new object independently. Keep the earlier occurrence's evidence historical. Paths can select a comparison Blob, and Check can support a plausible match; only full verification establishes exact identity. Apply section 3.3 for root adoption. |
| A stable, location-confirmed size or fingerprint differs from the compared Blob       | Rule that Blob out for this observed occurrence. Its new Blob is unidentified until Verify. Keep the mismatch visible even if an older verification is recent.                                                                                                |
| A fingerprint agrees and the location is confirmed                                    | Record a plausible match to the compared Blob, not a new exact association. Do not refresh verification time or erase a prior content contradiction for the continuing object.                                                                                |
| Verify recognizes a Blob and confirms its location                                    | Establish the verified occurrence at the read/check times. It may recover an existing Blob or identify different content; prior tags and revisions stay with their original Blobs.                                                                            |
| A read fails, is unstable, or cannot confirm its location                             | Current content or attachment remains unresolved. Preserve the specific failure and any valid unattached recognition. Retry or verify; an I/O failure is not a fingerprint mismatch.                                                                          |
| A file is checked on its own, including after replacement between listing and opening | Update only that file and the freshly observed anchors. Its opened observation is outside the earlier enumeration; parent and sibling coverage keep their own dates.                                                                                          |
| An identity appears elsewhere and the old path is invalid                             | Record a possible move with unconfirmed content. A reviewed candidate Blob can be checked or verified at the new path. Do not transfer tags, verification, or subtree coverage through an inode match.                                                        |
| A newer usable complete listing omits a name                                          | Remove the old attachment from that directory's current view. Retain history; this does not prove the Blob absent elsewhere.                                                                                                                                  |
| A listing is incomplete, fails, or was not requested                                  | Retain any older compatible listing as last-known, with the new limitation. New usable entries can be shown, but unseen names are not established absent.                                                                                                     |

A known content contradiction requires Verify to reestablish exact identity; a
later metadata agreement or fingerprint match does not erase it. Contradictions
are scoped to the compared Blob and observed object. They do not ban future
recognition of the same Blob in a restored or changed file.

When combining directory observations, apply section 7.1's binding, epoch,
location, native-identity, ancestor, and no-intervening-conflict requirements.
Replacement of a directory, or a change to a file or symlink, detaches old
subtree evidence. Unresolved ancestry stays unresolved. The frontend can show a
historical tree while offering Inventory to obtain fresh hierarchy observations.

Inventory normally obtains entry metadata without opening every regular file. An
apparent size discrepancy can be recorded as a reason to Check or Verify. To use
size alone to rule out a Blob at a location, obtain the stable handle and
location-confirmed size evidence required by sections 3.4 and 13.3; this reads
no file contents. Do not silently treat a racing directory-entry stat as a
verified occurrence or as accepted size exclusion.

This is a shared query rule, not advisor-specific reconciliation. Advisors
consume these states and suggest actions. A freshness policy can prioritize work
but cannot hide the recorded conflicts or create missing Blob identities.

### 7.4 Examples

**Directory replacement:** Scan S1 enumerates `/a` and sees `old.txt`. S2 lists
`/` and observes a different native directory at `/a`, without entering it.
Display `/a` as observed with contents unknown. Do not display `old.txt` as a
current child of the replacement. Its historical observation survives.

**Selected file verification:** S1 lists `/a`. S2 verifies `/a/new.txt`. Display
the newer verified file with its own provenance. S2 has not proved that other
names in `/a` remain present or that no additional names exist.

**Directory rename:** `/a` appears absent and `/b` has a matching native
identity. Suggest "Inventory `/b` to refresh its entries; it may be the former
`/a`." Preserve the possible relationship as evidence for the suggestion. Do not
refresh or relocate S1's descendant coverage through that hypothesis.

**Restore:** Scan the restored root as a candidate with new native identities.
Compare its fresh observations with a frozen reference view, and offer adoption
with the exception report described in section 3.3. Acceptance activates the
candidate binding and retains its checked epoch. Fresh listings reconstruct the
namespace; full reads recover existing Blob IDs. Sample matches remain uncertain
and old directory coverage does not carry over.

**Reboot:** After a reboot, the filesystem identity agrees and the root has the
same native identity, so the scan continues the existing identity epoch. The
earlier listing of `/a` and its files' verification ages still apply under the
usual continuity rules. The reboot itself refreshes nothing.

**Incomparable identity:** The archive is copied to a new filesystem, and a scan
observes `/a` in a new identity epoch. Return `unresolved` with
`identity_epoch_differs` for attachment of the old listing, even if the numeric
inode happens to agree. Do not report replacement from the epoch change. The
frontend can show the earlier contents as history; a fresh enumeration supplies
coverage for the new context.

**Separate freshness:** Inventory lists `a.pdf` and `b.pdf` on Monday. Verify
identifies both on Tuesday. Check finds a fingerprint mismatch for `a.pdf` on
Wednesday. Verify refreshes only `b.pdf` on Thursday. The view retains Monday's
listing date, Wednesday's changed/unidentified status for `a.pdf`, and
Thursday's verification for `b.pdf`. Its older Blob tags remain on the original
Blobs.

**Retry ordering:** A directory enumeration fails and a retry succeeds. Use the
retry's entries alone for complete coverage; keep the failure in history. If a
later requested read fails after an earlier successful read in the same run,
show that later failure with the earlier verification as last-known. Publication
order cannot make the earlier bytes newly verified.

## 8. Mixed Evidence and Absence Claims

A directory can have a complete listing, two verified files, three sampled
files, and one unreadable file. Report all of these facts:

```text
Listing complete at 14:02
2 files verified; 3 sample matches; 1 unreadable
Descendants: 1 directory unvisited
Suggested action: verify the 3 sampled files and retry the unreadable file
```

A failed read does not invalidate successful siblings. Verify on one child does
not refresh its parent. Directory summaries aggregate counts by evidence kind
and expose unvisited or stale coverage; they do not collapse everything to one
operation or "unchanged" badge.

Distinguish these questions. Claims about an observed occurrence at a location
require the location confirmation described in section 6.2; content recognition
of an unattached read object does not establish a scoped occurrence.

| Question                                                  | Evidence required                                                                                                     |
| --------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------- |
| Was a particular name absent from this directory listing? | A complete apparently stable listing of the relevant directory at the stated interval.                                |
| Does this observation contain Blob A?                     | Accepted verification recognizing A.                                                                                  |
| Does this observed file differ from A?                    | Accepted full recognition of another Blob, stable size mismatch, or compatible stable sample mismatch against A.      |
| Does A plausibly remain at its expected location?         | An observed entry and sample agreement against A, with any continuity uncertainty disclosed.                          |
| Was A ruled out throughout a scope?                       | Complete namespace/traversal evidence and an exclusion of A for every possible regular-file occurrence in that scope. |
| Is A absent everywhere now?                               | Ordinary scans cannot establish this timeless, global statement.                                                      |

Ruling out A in a subtree does not always require fully identifying every file:
size and sample mismatches against A can suffice for particular occurrences.
However, a no-baseline file, sample match, unreadable entry, unknown entry type,
stale directory, or unvisited subtree can leave a possible occurrence
unresolved. Check normally compares each file to its own prior Blob, so its
mismatch against B cannot automatically rule out A.

Even exhaustive scan evidence describes observations made over an interval.
Prefer "No occurrence of A found in the fully checked scope during this scan"
with its scope and assumptions. A directory's disappearance invalidates old
locations beneath it; it does not prove that their Blobs were deleted rather
than moved elsewhere.

### 8.1 Coverage queries return evidence and unresolved work

The interpretation component answers an explicit question about a Blob or
namespace slot within a requested scope and evidence boundary. Its result
contains:

- the conclusion: a verified occurrence was found, absence was established
  within the scoped observations, or the question remains unresolved;
- the root binding, epoch, publication boundary, and interpretation version;
- the requested scope, observation intervals, and any traversal exclusions;
- typed references to the evidence supporting each positive or negative finding;
- unresolved items, with locations, reason codes, and relevant source
  references.

For example:

```text
Question: was Blob A ruled out in /archive under this scan's boundary policy?
Conclusion: unresolved
Remaining checks:
    /archive/photos           directory_not_enumerated
    /archive/docs/report.pdf  content_unresolved
    /archive/other            ancestor_attachment_unresolved
```

An advisor can turn those blockers into a scan request for the affected files or
directories. The query identifies missing evidence; it does not persist or
execute a suggestion. A blocker can require a different kind of work: verifying
a file does not resolve an unenumerated parent or an incompatible ancestor.

A verified occurrence is enough to answer that an occurrence was found, but
remaining coverage gaps still accompany the result. An absence conclusion
requires all possible occurrences in the requested scope to be accounted for;
no-baseline files, sample matches, and failed location checks cannot fill those
gaps. Exclusions remain explicit, and the conclusion never extends into them.

## 9. Attempts, Publication, and Recovery

### 9.1 One scanner, explicit attempts

A scan takes exclusive application access to the catalog database for its whole
session, enforced by a process-lifetime lock. Other database users, including
scans, readers, advisor queries, and semantic operations, wait until the session
finalizes or exits. The scanner can read and write within its own session. Drain
existing database users before granting the lock. Other commands wait before
accessing the database; a frontend may display already loaded data with its
original age.

Allocate a scan sequence before work starts. Scans cannot overlap. The prototype
also finishes one filesystem attempt before beginning the next: an enumeration
finishes or fails before descendant listings or file reads begin, and retries
are sequential. This avoids a separate arbitration model for overlapping worker
results. Record start/end times and monotonic within-scan event order. Ordering
is reproducible but is not a filesystem snapshot; external programs can still
change the tree, so the handle and location checks remain necessary.

Exclusive ownership is an application rule, not a transaction held open during
hashing. Retain short SQLite transactions for durable progress and recovery. The
lock covers the complete session, including finalization and recovery.

Each directory enumeration and each file read is a distinct attempt. The
provisional default permits one retry after an unstable attempt. Permission
failures are reported without an automatic retry loop. Record the actual policy.
An attempt without a terminal result is unfinished and unusable.

Directory enumeration records handle identity and change metadata before and
after listing and checks the directory's location again. Reaching the end of the
listing with acceptable stability and a confirmed location check produces usable
complete coverage at that observed location. A completed listing with failed
location confirmation remains a listing of the observed object, not coverage
that can be attached to the current location. Failure of a child's metadata
lookup is recorded on that child; it need not make an otherwise complete name
listing incomplete.

A retry has its own enumeration ID and its own entries. Never merge entries from
different attempts into one complete listing. Descendant work must refer to the
exact parent observations that anchored it. Publication includes terminal
attempt results and validates their dependencies. The transition rules select
current and historical claims without combining arbitrary successful rows by
pathname.

Retry an observation only while its native object remains compatible. If the
entry or an ancestor was replaced, create fresh observations and resolve the
anchor chain again. Successful file reads or child listings under an incomplete
parent listing can still be published when their own location checks pass; their
validity does not manufacture missing coverage for the parent.

### 9.2 Durable staging and atomic publication

Append sessions, attempts, observations, results, and diagnostics in short write
transactions. Hashing and traversal happen outside SQLite write transactions.
These rows are retained but excluded from ordinary evidence views until their
scan is published. Publication is scoped to the scan's binding. Published
candidate scans are available for comparison and review; they do not change the
active binding or its default browsing view. Binding activation is a separate
user action.

At finalization, one transaction:

1. validates terminal attempts and their anchor relationships;
2. records the terminal results as inputs to the ordered transitions, including
   failures that must remain visible;
3. records scope outcomes and the scan's terminal outcome;
4. inserts the publication record.

After exclusive scan access ends, queries use a database read transaction and
one publication boundary. Atomic publication also prevents a crash from exposing
part of a scan as a committed view. SQLite transactions provide the database
publication boundary; they do not provide a filesystem snapshot. See
[SQLite's atomic commit documentation](https://www.sqlite.org/atomiccommit.html).

A run with outcome `succeeded` met every requested operation. Outcome
`finished_with_gaps` means that usable evidence exists but some requested work
failed, remained unstable, or was left uncovered. Both outcomes may publish;
per-directory and per-file evidence controls which conclusions are supported. An
intentionally limited scope is not a gap. A successful Check run can still leave
exact content unresolved because identifying every Blob was not requested.
Inventory can likewise succeed with no content reads. A successful retry can
complete the requested work while its earlier failure remains in history; an
unrecovered requested failure gives `finished_with_gaps`.

Verify with an unreadable requested file finishes with gaps. Accepted content
recognition with a failed location check also leaves requested location
verification unfinished. Publish that distinction when the run otherwise
qualifies for publication. Reserve `complete` and `incomplete` for explicitly
named coverage or work results, such as a directory listing; neither is a scan
operation or a run-outcome enum.

A fatal root-resolution, unexpected replacement of the root being scanned, or
integrity failure publishes no current-state evidence for that scan. Record a
`failed` outcome. A detected replacement can then be scanned under a separate
candidate binding for the adoption workflow; it must not silently replace the
active scan's root. An interrupted process leaves no publication. After
acquiring the scanner lock, recovery appends an `abandoned` outcome for an
unfinished session. Its completed and incomplete attempts remain available for
diagnostics; the prototype does not resume them or publish their successful
fragments automatically.

### 9.3 Blob admission and scan publication differ

A successful full read can admit a Blob and baseline in a short transaction
before the overall scan publishes. This is a durable fact about recognized
bytes, not a claim that a current location contains them. Record the exact
source read. If later work fails, retaining that Blob does not make the failed
scan's path observations current.

Content recognition can reuse such a Blob in later scans. A baseline must always
be backed by accepted full recognition, even if that read's location check
failed or its scan never published. File-read results therefore preserve content
status independently from location status. A failed or unstable content read
cannot admit a Blob or baseline, even if its location check succeeds. Extractor
outputs under section 4.2 are admitted the same way: with the accepted content
result, and never from a failed or unstable read.

This keeps exact content admission independent of location and unrelated
directory failures. Presence views additionally require scan publication and
accepted location attachment. Admission alone never fills a namespace or
content-coverage gap in such a view.

### 9.4 Closing attempts and scans

Completed evidence is closed to further additions as well as immutable.
Exclusive database access prevents concurrent writers; closure prevents a later
sequential writer from adding evidence to completed work.

An attempt is open only while it has no terminal result and its scan has no
terminal outcome. Directory entries may be appended only to an open enumeration.
Writing an attempt's terminal result closes it regardless of success or failure:
no later entries, measurements, or replacement result may be added to that
attempt. A retry uses a new attempt and fresh observations.

Scan finalization writes the remaining target results, diagnostics, terminal
attempt input sets, outcome, and any publication in one transaction. Once it
commits, reject new observations, attempts, results, diagnostics, extractor
runs, or selection rows owned by that scan. This applies to all outcomes,
including unpublished `failed` and `abandoned` scans. Publication belongs to the
finalization transaction; it cannot be added afterward to a closed scan.
Preserve the atomic Blob/result/baseline admission rule in section 9.3.

Recovery may append an `abandoned` outcome to a session without a terminal
outcome after acquiring exclusive access. Retain unfinished attempts as
unfinished; do not invent their missing measurements or successful results. The
scan's closure prevents those attempts from being resumed or completed later.
Recovery leaves already finalized sessions unchanged.

Closure does not prohibit referencing existing evidence. A file read may use an
observation from a completed parent enumeration without becoming another entry
in that enumeration. Finalization may select a completed attempt, and later
scans or user operations may reference old reads and baselines. These references
do not add measurements to, or change membership of, the closed work.

Enforce the rules through validated database write operations and schema
constraints or triggers where appropriate. The concrete DDL and transaction
ordering are implementation work, including how publication is permitted during
finalization but rejected afterward. No additional lifecycle table is required
by this design; existing terminal results and outcomes can express closure.

## 10. Advisors Suggest User Actions

An advisor is a heuristic for day-to-day operation. It looks for patterns in
occurrences, coverage, and freshness and suggests what the user could do next.
It reads the catalog only: it never reads file content, and it produces no
metadata about Blobs, which is an extractor's job under section 4.2.

Advisor output is a proposed action, not an automatically committed assertion
about identity or ancestry. Suggestions are computed from retained evidence and
shared interpretation results. They are transient derived output by default;
creating or presenting a suggestion requires no database write. Each suggestion
contains:

- a concrete action and its target scope, candidate binding, or semantic
  objects;
- the supporting observations, reads, and enumeration results;
- a plain-language reason and the applicable evidence category;
- unresolved assumptions and what the action would resolve;
- advisor and interpretation versions, input root and epoch, input publication
  boundary, relevant semantic versions, and action preconditions.

Examples:

| Evidence                                                                            | Suggested action                                                   |
| ----------------------------------------------------------------------------------- | ------------------------------------------------------------------ |
| A file's partial sampling plan has no compatible baseline.                          | "Verify this file to identify its content."                        |
| Three files differ from their prior baselines.                                      | "Verify these three files."                                        |
| A directory listing is usable but a child is inaccessible.                          | "Restore access and Inventory this child directory."               |
| A different native directory occupies a known path.                                 | "Inventory the replacement directory; its entries are unknown."    |
| A was verified earlier and B later at the same location.                            | "Review adding B as a revision after A in this Lineage."           |
| Numbered files have fully recognized contents.                                      | "Review creating this proposed revision sequence."                 |
| A file's bytes occur in several unrelated Lineages.                                 | "Choose the relevant Lineage before applying contextual metadata." |
| Sidecar files sit beside same-named verified files and satisfy a registered schema. | "Review linking each sidecar to the file beside it."               |
| An expected path of a bundle holds different content.                               | "Check this bundle; it is no longer intact."                       |

An action specifies Inventory, Verify, or Check over selected files, a directory
scope, or the entire root. An advisor may group related unresolved items into
one actionable request. It must not silently turn sample agreement into revision
membership, copy tags after an edit, or decide that a directory was renamed.

Acceptance of a scan suggestion creates a durable scan request. Acceptance of a
semantic suggestion executes a validated semantic operation. Acceptance of a
link suggestion submits the proposed link statement under section 12.2.
Acceptance of an adoption suggestion activates the reviewed candidate binding
under section 3.3. In the same transaction, preserve a snapshot of the accepted
proposal, its evidence references, uncertainties, advisor and interpretation
versions, and relevant input boundaries with the resulting action. Direct user
actions need no advisor proposal. Merely generated or displayed suggestions have
no mandatory persistent identity or lifecycle.

Before first executing an accepted action, recheck the input publication,
binding, epoch, interpretation version, and semantic preconditions. If relevant
evidence has changed, recompute the proposal or require review of the changed
result. Filesystem targets are resolved anew when the requested scan runs.
Suggestions are never commands to act on an unchecked inode or stale pathname.
Recording acceptance must be atomic with the resulting request or operation, so
an audit record cannot claim an action was accepted without its durable result.

Dismissal, deferral, or an inbox that survives restarts are optional porcelain
workflows. If introduced, persist the user decision and enough proposal context
to explain it; do not make every advisor output a permanent plumbing object.
Suggestions can otherwise be regenerated from retained facts. This boundary is
about the distinction between derived proposals and user actions, not a limit on
history retention. Once an action and its provenance are recorded, retain them.

Acceptance does not increase the certainty of the underlying scanner evidence. A
user can establish a semantic relationship, but cannot convert a sample match
into verification by approving it.

An advisor may read derived metadata, for example a perceptual-similarity
result, to motivate a suggestion. Similarity never establishes Blob identity.

### 10.1 Command acceptance and database witnesses

Retry handling belongs to a shared command acceptance layer, outside the core
catalog model. This layer handles direct user commands and accepted advisor
proposals. The server is the authoritative command handler; this contract does
not require a network service.

Use a server-issued, unique, single-use witness for an operation type. The
client adds its own unique request ID and submits the witness, client request
ID, and operation payload. Keep the client request ID even though the witness is
already unique. It identifies a submission, not a persistent client or a catalog
object. The client preserves the same witness, request ID, and payload for
retries; an intentional repeat obtains a new witness and uses a new request ID.

On first acceptance, use one database transaction to validate the witness and
its operation type, the payload, and all action preconditions, perform the
durable operation, append any applicable accepted-proposal provenance, and
record the witness's consumption with the client request ID, accepted payload,
and durable result. The application ledger must contain enough information to
compare a retry and return that result. Serialize consumption so competing
submissions cannot commit two operations using one witness. A rejected request
or rolled-back transaction creates no consumption record or partial operation.

For a consumed witness, an identical operation type, client request ID, and
payload return the original recorded result. Reuse with a different operation
type, request ID, or payload fails. Resolve this case before revalidating the
original action's preconditions: later catalog changes do not invalidate an
acknowledgment of work already committed. A response lost after commit therefore
causes neither a duplicate action nor an ambiguous "already used" response.

For a scan, the durable operation is creation of the queued scan request. Its
later execution and outcome are separate; a retry returns the same request and
can query its status without starting another scan. For semantic changes and
root adoption, the result identifies the committed semantic operation or binding
activation. Witness records and results survive restart and remain available for
retries. Witness handling follows section 9.1's exclusive database access rule;
it does not hold a transaction open while scanning.

Keep witness persistence in application infrastructure within the same SQLite
database so it can share the operation's transaction. Core Blob, scan, revision,
and binding records need no witness or client-request fields, and their validity
does not depend on this ledger. Do not use Blob/revision equality to deduplicate
submissions. Token format, issuance representation, versioned payload
comparison, and API encoding are implementation details of the command layer.

## 11. Lineages, Revisions, and Tags

A Lineage represents user meaning across exact contents. Each revision belongs
to one Lineage and references one Blob established by an accepted full read.
Revisions have stable public IDs and optional labels such as `v12`. Filenames
and labels are not revision identities.

Parent edges form a DAG within one Lineage. Multiple roots and leaves are
allowed. Cross-Lineage edges and ordered-parent semantics are omitted in the
prototype. A Blob may appear repeatedly within one Lineage, including a return
to earlier identical bytes. Do not impose uniqueness on `(lineage_id, blob_id)`.
There is no mandatory "current revision"; a preferred revision can be a later,
explicit semantic feature.

Revision-source links record observations that motivated or manifested a
revision. Other locations with identical bytes are discoverable occurrences of
its Blob, not automatically designated manifestations of that Lineage. Raw path
browsing shows Blob evidence and Blob tags, and can offer links to relevant
Lineage contexts without applying all their tags to the path.

Tags can be assigned separately to Blobs, Lineages, and revisions. Revision tags
allow "approved in this Lineage at this revision" without classifying every
occurrence of identical bytes or every revision in the Lineage.

An unidentified filesystem occurrence cannot be used to assign Blob tags or
create a revision: offer Verify first. A fingerprint match alone is insufficient
to establish that exact association. Users can still tag an already known Blob
or edit a known Lineage/revision without a current file occurrence. Scanning
never moves those assignments onto unidentified replacement content, and lost
location evidence never deletes the semantic records. Verification of available
bytes can recover their existing Blob and its metadata later.

Retain the exclusive-hierarchy model: each hierarchy is one tag tree, and each
target can explicitly select at most one node from it. A selected descendant
satisfies ancestor queries. Independent classifications use independent
hierarchies. This deliberately does not support selecting both Alice and Bob
inside one People hierarchy. Multi-valued hierarchies would be a distinct future
feature, not an implicit exception to the uniqueness rule.

In a revision view, show Blob, Lineage, and revision assignments with their
origins. Different selections from the same hierarchy in different scopes are
retained and shown as conflicting contextual assignments. Do not silently merge
or override them. Queries specify the assignment scope; an explicit any-scope
query may search all three. There is no synthesized single effective tag value.

Tag IDs remain stable across renaming and reparenting within a hierarchy. The
prototype uses exact, case-sensitive Unicode names for semantic tags, with one
root per hierarchy and unique sibling names. Reject cycles and cross-hierarchy
reparenting. These semantic name rules are separate from filesystem byte names.

Semantic operations update a current projection and append a change record in
the same transaction. Record creation, assignment changes, edge corrections,
renames, and tombstones with enough prior and resulting values to explain and
reverse an ordinary edit. Revision content identity is immutable; correcting
that identity creates another revision and tombstones the erroneous one. Link
statements, schema registrations, and extractor-run retractions are semantic
operations too; section 12 defines them.

Blobs and scanner history have no automatic garbage collection. An explicit
purge is outside the first prototype; do not implement cascading deletion as a
substitute. Retain semantic history and stable IDs when objects are tombstoned.

## 12. Manifests, Links, and Bundles

A manifest is a file that describes other files: a sidecar beside one video, an
export listing thousands of entries, the file list of a mirrored directory, a
package index. Three separate things are recorded about it, and each has its own
source:

| Part             | Source                                          | Nature                                                                 |
| ---------------- | ----------------------------------------------- | ---------------------------------------------------------------------- |
| Manifest records | An extractor, from the manifest Blob's bytes    | Derived data: a function of those bytes.                               |
| Link statement   | A command submitted to Filestore                | An assertion that the manifest describes certain Blobs.                |
| Bundle           | A link statement that also fixes relative paths | A declared identity for a group of files whose layout must not change. |

These are independent steps. A manifest can exist with no links. A manifest can
be linked to the Blobs it describes without any bundle, and most sidecars are. A
bundle is declared only where the layout itself is part of what must be
preserved.

Nothing in this section adds a file to the managed tree. Manifest files are read
where they sit, and statements exist only in the catalog.

### 12.1 Manifests and schemas

A manifest is a Blob whose content an extractor has rendered as JSON records and
Filestore has validated against a registered schema. The file itself is plain:
it carries no Filestore envelope and no Blob IDs.

Rendering follows section 4.2. Two built-in extractors cover files that are
already JSON: `json` renders a JSON document as one record, and `jsonl` renders
JSON Lines as one record per line. A manifest in any other format is rendered by
a converter that the user supplies and that runs as an extractor on the verified
original. Filestore knows no manifest formats. Because the records are extractor
output, they carry the extractor's identity and run, and section 4.2's rules for
reproduction, conflicts, and corrections apply to them.

A schema is a Blob holding a JSON Schema. The built-in `json-schema` extractor
parses it and checks that it is a valid schema in the dialect Filestore pins.
References to remote resources are rejected, not fetched. A user operation then
registers the parsed schema under a name and version, which are unique as a
pair. A later revision of a schema is registered under the same name with a new
version.

A Verify request marks a file target as a manifest by naming the extractor, its
configuration, and a registered schema. When the read is accepted, every record
is validated against that schema. Validation is all or nothing: if any record
fails, no manifest is created, the file remains an ordinary Blob, and a
diagnostic lists the failing records. The request is the durable record of the
user's decision to treat the file as a manifest.

There is one manifest per Blob, extractor identity, and schema, and each has a
public ID. A manifest that describes other manifests needs nothing special: a
schema is a Blob, and a link may name any Blob, including a manifest.

Store one row per record, with its data as SQLite JSONB. Every record of a
schema passed the same validation, so a path such as `data ->> '$.taken_at'` is
known to exist with the declared type. Paths use SQLite's syntax, which has keys
and array indexes but no wildcards, filters, or recursive descent. Searching
inside one record uses `json_each` or `json_tree`. A JSON Lines manifest makes
each entry a row, so searching its entries is an ordinary filter. Indexes on
chosen paths are an optimization and never change what is stored. SQLite limits
a record to 1 GB and 1,000 levels of nesting by default; implementation sets
lower operational limits.

Manifest records are a copy of what the manifest file says, kept in the catalog.
Section 4.1 already treats the catalog as being as sensitive as the files.

### 12.2 Link statements

A link statement asserts that a manifest describes a set of Blobs. It is
submitted as a command, as JSON Lines on standard input or through the command
interface, and it is never read from the managed tree. An accepted statement is
stored in the catalog, recorded as a semantic operation, and kept in history.

#### Format

```text
{"filestore_links": 1, "manifest": "<manifest id>", "submitter": {"name": "ia-linker", "version": "0.3.1", "configuration": "default"}, "base": "mirror/crab"}
{"path": "crab.mp4", "entry": "$.files[0]", "role": "original"}
{"blob": "<blob id>", "entry": "$.files[1]"}
```

The first line is the header:

| Member            | Required | Meaning                                                                                      |
| ----------------- | -------- | -------------------------------------------------------------------------------------------- |
| `filestore_links` | yes      | Format version; `1`.                                                                         |
| `manifest`        | yes      | Public ID of the manifest the statement is about.                                            |
| `submitter`       | yes      | Name, version, and configuration of the program that produced the statement.                 |
| `base`            | no       | A root-relative directory. Line paths are relative to it; without it they are root-relative. |
| `bundle`          | no       | `true` declares a bundle under section 12.3.                                                 |
| `observed_since`  | no       | A publication sequence. Paths resolve only from observations published at or after it.       |
| `replaces`        | no       | Public ID of an earlier statement that this one supersedes.                                  |

Every later line links one Blob:

| Member   | Meaning                                                                          |
| -------- | -------------------------------------------------------------------------------- |
| `blob`   | Blob ID of the described content.                                                |
| `path`   | Path of the described file.                                                      |
| `record` | Ordinal of the manifest record the link refers to.                               |
| `entry`  | A path inside that record, in SQLite JSON path syntax.                           |
| `role`   | A label for how the file relates to the record. Filestore does not interpret it. |

A line names its file by `blob` or by `path`. Without `record` and `entry`, the
link is to the manifest as a whole. `entry` without `record` is allowed only
when the manifest has one record. A named record must exist, and a named entry
must resolve in it. Unknown members and duplicate lines are rejected.

#### Naming a file by Blob ID

A `blob` line links exactly the named Blob. It is valid when the catalog knows
that Blob. No filesystem evidence is consulted, so the submitter alone is
responsible for naming the right content.

#### Naming a file by path

A `path` line is resolved to a Blob from evidence already in the catalog. A
statement never reads the filesystem. The path resolves to Blob B when all of
these hold in the active binding's current view at the moment the statement is
committed:

1. The Location has a current occurrence, and it is a regular file.
2. The occurrence is verified as B: an accepted full read recognized B with a
   confirmed location.
3. Nothing later qualifies that verification. The occurrence's latest metadata
   observation agrees with the verifying read's opened observation in native
   identity, size, and modification and change timestamps. No later comparison
   contradicts B, and no later read or location check of the occurrence failed
   or remains unresolved.
4. If the header gives `observed_since`, the occurrence's latest metadata
   observation was published at or after that sequence.

Otherwise the line is unresolved, with one of these reasons:

| Reason                        | Meaning                                                                             |
| ----------------------------- | ----------------------------------------------------------------------------------- |
| `location_unknown`            | There is no current occurrence at the path, or it is not a regular file.            |
| `occurrence_unidentified`     | The file there was never verified, or it replaced the file that was.                |
| `modified_since_verification` | The file may have been modified: a later observation differs in size or timestamps. |
| `content_contradicted`        | A later size or sample comparison ruled out the verified Blob.                      |
| `evidence_unresolved`         | A later read or location check failed or was unstable.                              |
| `observation_too_old`         | The latest observation predates `observed_since`.                                   |

Every reason has the same remedy: Verify the path, then submit the statement
again. A frontend that wants a fresh look before linking submits an Inventory or
Verify request for the named paths and passes that scan's publication sequence
as `observed_since`. Inventory is enough to notice a file that may have been
modified; Verify also establishes what it holds now. A resolved line records the
read that verified its Blob and the observation that showed the file unchanged.

#### Acceptance

A statement is accepted or rejected as a whole. If the header is invalid, the
manifest is unknown, or any line is invalid or unresolved, nothing is recorded,
and the result lists every failing line with its reason. On acceptance, the
statement, its links, and its semantic operation are committed in one
transaction, together with the publication sequence that its paths were
evaluated against. Statements pass through the command acceptance layer of
section 10.1, so a retried submission returns the original result.

Every statement names its submitter. A tool identifies itself by name, version,
and configuration, in the same form as an extractor identity; the tool that
links a manifest is often the converter that rendered it. A frontend submitting
on a user's behalf identifies itself the same way, as does an accepted advisor
proposal.

#### Scope

A link belongs to its statement and its manifest. One Blob can be linked by many
statements about unrelated manifests, because identical bytes found in unrelated
places are one Blob. Links from different statements are never merged, and none
overrides another. A view of a file lists every current link of its Blob, each
with its manifest and statement. Where a bundle occurs at the file's Location,
that bundle's links are marked as the ones that apply there.

#### Corrections

An accepted statement is immutable. A correction is a new statement whose header
names the earlier one in `replaces`. Both remain stored. The earlier one is
superseded as a whole, in the same transaction that accepts the new one. A
statement can be superseded only once. The replacement may name a different
manifest, for example a corrected file or a rendering by a newer converter. A
user operation can also withdraw a statement without replacing it.

Current links are those of statements that are neither superseded nor withdrawn.
Default queries use them, and history remains queryable. When a manifest is
rendered again by a newer extractor version, statements about the earlier
rendering stay current until they are replaced. An advisor can suggest the
replacement.

### 12.3 Bundles

A bundle is a manifest together with files whose relative layout is fixed: a
mirror of a public directory, a tree bound by a Merkle root, a software package.
A link statement whose header sets `bundle` declares one. Not every linked
manifest is a bundle. Declare one only when the layout is part of what must be
preserved.

In a bundle statement, every line has a `path`, which is the file's path inside
the bundle. Paths are relative, contain no parent references, and are unique
within the statement. A line may also give `blob`. The Blob is then taken as
stated, and the path is recorded as layout without being resolved. A line
without `blob` is resolved under section 12.2 at `base` joined with `path`, so
`base` is required whenever such a line is present. Outside a bundle, a line
that gives both `blob` and `path` is rejected. The manifest file may itself be
one of the entries.

A bundle is therefore a fixed mapping from relative path to Blob, together with
its manifest. It is identified by its statement and defined by content, like a
Blob. It has no Location of its own and can occur at one base directory, at
several, or at none. `base` is kept as evidence of where the bundle was when it
was declared. Bundles may nest or overlap. Replacing a bundle's statement
declares a new bundle that supersedes it.

Checking a directory against a bundle reuses the comparison of section 3.3. A
Check or Verify request over a base directory may name a bundle as its
comparison reference. The expected file at each relative path is the entry's
Blob, and each expected path is classified exactly once as a verified match,
sample match, changed, missing, or unresolved. Files under the base that the
bundle does not list are counted as added. The bundle occurrence at that base
is:

| State              | Condition                                                 |
| ------------------ | --------------------------------------------------------- |
| `intact`           | Every expected path is a verified match.                  |
| `plausibly intact` | Every expected path matches, some of them only by sample. |
| `damaged`          | At least one expected path is changed or missing.         |
| `unresolved`       | Otherwise.                                                |

Added files are reported beside the state and do not change it.

Filestore does not prevent changes inside a bundle, because it does not
intercept filesystem operations. It detects them, and an advisor can flag a
bundle occurrence that is no longer intact. Filestore does not interpret a
Merkle root, checksum list, or signature found in a manifest. Its own check is
against Blobs; verifying a manifest's internal integrity claims is the
submitter's concern.

## 13. Provisional Database Schema

Use logical `plumbing`, `metadata`, `advice`, and `semantic` groups in one
SQLite database. Use strict tables, foreign keys on every connection, exact
length and enum checks, explicit referential actions, and transactional
validation for graph invariants. Integer primary keys serve internal joins.
Externally referenced roots, scans, observations, Blobs, Lineages, revisions,
and tags have unique random 32-byte public IDs, as do queued scan requests,
extractor runs, registered schemas, manifests, and link statements. Transient
advisor suggestions do not require catalog-wide public IDs.

This schema describes the core catalog. Section 10.1's application witness
ledger shares this database and its transactions but is specified separately by
the command layer. It does not add retry identities or dependencies to the core
tables below.

Below, `?` means nullable. A metadata snapshot includes native identity, type,
size where applicable, modification/change timestamps, timestamp precision, and
optional generation information. Time intervals are required for completed work;
wall time is accompanied by a within-scan monotonic event order. Versioned
structured encodings must be specified before implementation.

### 13.1 Root configuration and scanning

```text
database_metadata
    id (= 1), schema_version, content_scheme_version, created_at

roots
    id, public_id, created_at

root_bindings
    id, root_id, root_relative_components,
    native_root_identity, reason, created_at

root_binding_activations
    id, root_id, root_binding_id, previous_activation_id?, identity_epoch_id,
    reason, reference_view?, comparison_publication_sequence?,
    comparison_report?, created_at

identity_epochs
    id, root_binding_id, filesystem_context, observed_root_identity,
    reason, created_at

scan_requests
    id, public_id, root_binding_id, operation, requested_policy, comparison_reference_view?,
    requested_at

scan_sessions
    id, public_id, request_id, scan_sequence, root_binding_id, identity_epoch_id,
    operation, sampling_policy_id?,
    boundary_policy, retry_limit, scanner_version, started_at

scan_targets
    id, request_id, relative_components, target_kind, directory_depth?, comparison_blob_id?

scan_target_results
    scan_id, target_id, resolution_status, anchor_observation_id?,
    traversal_status, operation_status, finished_at, diagnostic_id?

scan_outcomes
    scan_id, outcome, finished_at, summary_counts, error_summary?

scan_publications
    scan_id, publication_sequence, published_at

scan_diagnostics
    id, scan_id, observation_id?, code, severity, message,
    native_path_hint?, occurred_at, event_order
```

`operation` is `inventory`, `verify`, or `check` on both request and session.
Sessions retain the actual sampling policy. Check requires a policy; Verify
requires one when baseline creation is enabled. Inventory requests no content
reads or baseline creation and does not apply sampling settings. There is no
small-file threshold field: section 6.3's rounded plan determines the full-read
fallback. Target results distinguish requested operation completion from content
work intentionally not requested. `comparison_blob_id` is optional for an
individual Check target and references an existing Blob. It overrides the usual
historical-location lookup without asserting identity; the read records the
actual comparison and compatible baseline. Directory targets do not use this
override. Conflicting overlapping requests must be resolved before execution.

The prototype has one logical root. Its active binding is selected by the latest
activation in a validated activation chain, not by the latest binding row. A
binding with no activation is a candidate and may own scans and identity epochs.
Bindings, epochs, and activation records are immutable. Activation requires the
candidate epoch to belong to the activated binding and that binding to the same
logical root. Serialize activations and reject a stale previous-activation
reference so two concurrent acceptances cannot fork the active chain.

The first activation can be initialization without a comparison.
Comparison-based adoption records the frozen reference view, candidate
publication boundary, and versioned reviewed report: counts, threshold policy,
coverage, structural changes, exceptions, and unresolved scope. Evidence
references are retained in the report and, for an accepted advisor proposal, its
action provenance. An explicit adoption outside the recommendation records the
user's decision and the same available limitations. Epoch changes never imply a
binding activation by themselves.

Each scan has at most one outcome and publication; publication requires a
`succeeded` or `finished_with_gaps` outcome in the same transaction. Target
results describe traversal and policy completion, not just whether the target's
initial lookup succeeded. Requests contain the normalized targets and requested
policy; sessions retain the actual policy. A candidate comparison request also
retains its frozen reference view; this is input to baseline selection and
cannot silently change while the scan runs. The reference may instead be a
bundle and a base directory under section 12.3; a bundle is immutable, so it is
frozen by nature. A target result must belong to its session's request. A queued
request is bound to the binding the user reviewed, whether active or a
candidate. A change of that target before execution requires a new or explicitly
reconfirmed request, not silent retargeting. Publication and scan sequences are
distinct from wall time.

The lifecycle rules in section 9.4 constrain inserts as well as updates. Schema
implementation must enforce one terminal outcome and at most one publication per
scan, validate the finalization transaction, and reject later inserts into every
table owned by a closed scan. Publication and selection rows are part of this
closure. A foreign key proving that a scan exists is insufficient to prove it is
still open. Decide the exact constraints, triggers, and write protocol when
implementing the schema, and cover normal completion, failure, and recovery.

### 13.2 Observations and enumeration attempts

```text
filesystem_observations
    id, public_id, scan_id, parent_observation_id?, native_name,
    role, enumeration_attempt_id?, metadata_status,
    metadata_snapshot?, observed_at, event_order

directory_enumeration_attempts
    id, scan_id, trigger_observation_id?, directory_observation_id?, attempt_number,
    before_metadata?, started_at, start_event_order

directory_enumeration_results
    attempt_id, listing_status, stability_status, after_metadata?,
    location_check_status, location_reason_code?, failed_component?,
    location_checked_at?, location_check_event_order?,
    entries_seen, finished_at, end_event_order

selected_directory_results
    publication_scan_id, attempt_id
```

An observation's parent belongs to the same scan and denotes a directory. Root
observations have no parent and use an explicit root role. Ancestor anchors can
exist without an enumeration. Observations from a listing reference the exact
enumeration attempt and its directory observation as parent. Preserve
unknown-type entries when only their names were obtained.

Role `opened_handle` records a fresh handle observation with no enumeration
membership. Attempt links distinguish it from a prior trigger observation; they
do not assert permanent object continuity. Its parent anchors and scan must be
consistent, and all initial measured properties come from that handle. A later
observation or attempt never fills unknown metadata fields in a prior sighting.
An opened root uses the root role with these same ownership rules.

For enumeration, `directory_observation_id` references the freshly opened
directory. It is absent only when opening failed; such an attempt retains a
trigger and a failed result. If neither observation exists, record a target
failure instead. Trigger and opened references must belong to the attempt's
scan. Listing entries and before/after metadata belong to the opened directory,
never to a different trigger object. No entries can reference an attempt that
has no opened directory observation.

An enumeration attempt is inserted after open resolution and before streaming
its entries. Its terminal result is appended after the attempt ends. There is at
most one terminal result per attempt. A retry gets a new attempt and new entry
observations. Duplicate native names within an attempt prevent that attempt from
being selected as a complete stable listing; do not silently deduplicate a
racing enumeration.

An entry insert must check that both its enumeration and owning scan remain
open. A terminal enumeration result closes entry membership immediately, before
scan finalization. Subsequent child work can reference those entries under
section 9.4 without appending to that closed listing.

Selection rows identify terminal results from the published scan as inputs to
section 7.3's transitions; include all terminal attempts, including failures.
They do not designate a single successful winner by pathname. A directory
observation has at most one selected attempt because each open has its own
observation. Overlapping target work is normalized, and finalization validates
the anchors of each attempt. Successful retries cannot inherit children from a
failed attempt. Publication does not assert coherence across filesystem times.
The interpretation component requires complete listing status, accepted
stability, and a confirmed location check before using an enumeration for
location-scoped absence. A terminal result can be selected to expose failure or
unresolved status without satisfying those requirements.

### 13.3 File reads, Blobs, and baselines

```text
file_read_attempts
    id, scan_id, trigger_observation_id?, opened_observation_id?, attempt_number, method,
    compared_blob_id?, comparison_source_read_id?, baseline_id?,
    before_metadata?, started_at, start_event_order

file_read_results
    attempt_id, content_status, content_reason_code?, after_metadata?,
    bytes_read, recognized_blob_id?, content_finished_at, content_end_event_order,
    location_check_status, location_reason_code?, failed_component?,
    location_checked_at?, location_check_event_order?, finished_at, end_event_order

selected_file_results
    publication_scan_id, attempt_id

blobs
    id, public_id, content_digest, exact_size, scheme_version,
    admitted_from_read_id, created_at

sampling_policies
    id, scheme_version, canonical_policy, created_at

blob_sample_baselines
    id, blob_id, sampling_policy_id, sample_token,
    created_from_read_id, created_at
```

`opened_observation_id` points to a fresh regular-file handle observation from
the same scan. Its metadata supplies the read's before-state; if
`before_metadata` is also stored on the attempt, it must agree with that
snapshot. The recognized Blob, sampled comparison, after-metadata, byte count,
and location outcome all belong to this read and opened observation. They never
become properties of the trigger observation.

A byte read or accepted size check requires an opened observation. An unreadable
open has no opened reference, handle-derived metadata, content token, or Blob
association. Its result records `unreadable`, zero bytes read, and the
diagnostic. At least a trigger or an opened observation identifies a file-read
attempt; a failed target resolution without either is recorded as a target
failure. Trigger and opened references must belong to the attempt's scan. The
attempt is inserted after opening succeeds or fails, so these immutable
references are known before content work starts.

`content_status` includes `full_recognition`, `sample_match`, `sample_mismatch`,
`size_mismatch`, `no_baseline`, `unreadable`, `unstable`, and `integrity_error`.
A conflicting baseline uses `integrity_error` with reason `baseline_conflict`;
it is not a `sample_mismatch` or an accepted content result. `method` includes
full read, sample, and metadata-only check. Not-attempted content is represented
by no read attempt and an explicit scope/policy status. A no-baseline or
size-mismatch check can complete with zero content bytes read. Successful
recognition and comparison statuses require their respective content or metadata
stability checks to pass; success of the location check cannot compensate for an
incomplete or unstable content read.

`location_check_status` is independently `confirmed`, `contradicted`,
`unresolved`, or `not_checked`. Preserve its reason, failed path component when
applicable, and check time. A completed check has a timestamp and event order;
`not_checked` has neither. Location outcomes use the meanings in section 3.4.
Directory enumeration location checks use the same categories and retain
corresponding reasons and timing.

Only `full_recognition` results have `recognized_blob_id`. A sample result
requires a baseline whose Blob is the compared Blob and whose policy matches the
read's scan policy. Size mismatch requires a compared Blob and accepted size
evidence, but no baseline. Store the source full-read reference when the
comparison target was selected from historical location verification. Neither a
sample result nor a location check can populate `recognized_blob_id`.

Verification at a location is computed from accepted full recognition plus
`confirmed` location status, with the read's observation anchors. Its use in a
presence view additionally requires publication and compatible attachment to
that view. The full-recognition/contradicted-location combination is valid raw
evidence, not an integrity error; it must never appear as location verification.
Sample and size comparisons with unconfirmed locations similarly cannot rule out
a Blob at the queried location.

Public Blob IDs and content digests are independently unique. A version-1
sampling policy stores section 6.3's exact 48-byte canonical encoding; its
embedded version must agree with `scheme_version`. Canonical policy encodings
are unique. A policy's database ID is not part of the position or token input.
`(blob_id, sampling_policy_id)` is unique for baselines. Reuse of a baseline
requires token equality; a differing token is an integrity error with no
overwrite. Record a diagnostic identifying the existing Blob and policy without
including either token. Admission inserts or recognizes a Blob and appends the
content result, any baseline, and any extractor outputs in one transaction. The
admission source references a read attempt already inserted; deferred
constraints or transactional validation enforce its accepted full recognition
result, independently of location status. Do not introduce an insertion cycle
that requires mutating immutable Blob rows.

Result insertion requires an open attempt and scan; uniqueness of `attempt_id`
allows only one terminal result. Admission of a Blob or baseline from that read
belongs to the same transaction as its accepted content result. A later scan
cannot attach newly measured results to the closed read attempt, even if it
recognizes the same Blob. Existing source references remain usable.

For a published scan, every terminal file result has a selection row. The
ordered transitions derive the latest assessment and historical verification. A
successful result obtains its object association through
`opened_observation_id`, never through `trigger_observation_id`. An unsuccessful
open retains its trigger as provenance but has no opened-object association.

Relations across scan IDs, baseline policies, and compared/recognized Blobs
require composite foreign keys where practical and explicit transactional
validation elsewhere. Failed or unstable content attempts retain diagnostics but
no reusable computed content tokens. A successful content result with a failed
location check retains its recognized Blob or recorded comparison result.
Selected outcomes preserve such failures for the interpretation component to
report rather than substituting an older successful location result.

### 13.4 Extractor runs, derived metadata, and manifests

```text
scan_extractions
    id, request_id, target_id?, extractor_name, configuration, schema_id?

extractor_runs
    id, public_id, scan_id, extraction_id, extractor_name, extractor_version,
    configuration, started_at, retracted_at?

blob_derived_metadata
    id, blob_id, run_id, source_read_id, result_version, payload, created_at

manifest_schemas
    id, public_id, derived_metadata_id, name, version, semantic_operation_id,
    created_at

manifests
    id, public_id, blob_id, run_id, schema_id, record_count, source_read_id,
    created_at

manifest_records
    id, manifest_id, ordinal, data
```

`scan_extractions` belongs to a Verify request and records which extractors the
user asked for. A row without `target_id` applies to every file the request
reads in full; a row with one applies to that file target. `schema_id` is
present exactly when the extraction renders a manifest, and it is the durable
record of the decision to treat that file as a manifest. A run belongs to one
scan and one requested extraction, and it adds the version that actually ran.
Runs are owned by their scan and close with it under section 9.4.

Every output references its run and the read that supplied its bytes. That read
must have an accepted full-recognition result for the output's Blob,
independently of its location status; a path association or sample match
attaches nothing. Outputs are admitted in the same transaction as the content
result. They follow section 9.3: they are facts about recognized bytes and do
not depend on the scan's publication.

For one Blob and one extractor identity there is at most one derived-metadata
record, and for one Blob, extractor identity, and schema at most one manifest.
The identity is the run's name, version, and configuration, so enforce this by
transactional validation. A run that reproduces an existing output inserts
nothing. A run that produces a different one records an
`extractor_output_conflict` diagnostic and inserts nothing for that Blob.
`retracted_at` is set by a semantic operation and only affects default views.

A registered schema points at the derived-metadata record that the `json-schema`
extractor produced for its Blob. `(name, version)` is unique, and a
derived-metadata record is registered at most once. A manifest has
`record_count` records with ordinals from zero; `(manifest_id, ordinal)` is
unique. `data` is SQLite JSONB that passed the manifest's schema. A manifest is
inserted with all of its records or not at all.

Section 4.2 states the extractor contract and section 12.1 the manifest rules.
These tables depend on Blobs, reads, and scans, and nothing in plumbing depends
on them.

### 13.5 Accepted-action provenance

```text
action_provenance
    id, scan_request_id?, semantic_operation_id?, root_activation_id?,
    advisor_name, advisor_version, interpretation_version,
    input_root_binding_id, input_identity_epoch_id, input_publication_sequence,
    semantic_preconditions, accepted_proposal, accepted_at

action_evidence
    id, action_provenance_id,
    observation_id?, enumeration_attempt_id?, read_attempt_id?
```

There are no mandatory suggestion, suggestion-evidence, or suggestion-decision
tables. Suggestions are derived responses. `action_provenance` exists only when
an advisor proposal results in a user action. Exactly one of `scan_request_id`,
`semantic_operation_id`, and `root_activation_id` is set, and each resulting
action has at most one provenance record. The record and its evidence links are
appended atomically with that request, semantic operation, or binding activation
and retained afterward. Adoption proposals include both the reference and
candidate contexts in the accepted snapshot.

`accepted_proposal` is a versioned snapshot containing the action and targets,
rationale, evidence categories, uncertainties, scope, and preconditions actually
reviewed. Semantic references use stable public IDs and are validated when the
action is executed. Each `action_evidence` row references exactly one typed
evidence record. Store the raw sources needed to explain the accepted decision,
not just a reference to a disposable projection or transient suggestion ID.

These provenance records depend on plumbing facts and user operations; scanner
facts do not depend on them. Provenance records the reviewed decision; section
10.1's witness ledger records command acceptance and retry results. Direct
commands still use that ledger without needing advisor provenance. Optional
frontend persistence for dismissal, deferral, or a review inbox can be designed
separately without making suggestion storage a requirement for scanning or
interpretation.

### 13.6 Semantic identities and changes

```text
lineages
    id, public_id, title?, created_at, tombstoned_at?

revisions
    id, public_id, lineage_id, blob_id, display_label?,
    created_at, tombstoned_at?

revision_parents
    revision_id, parent_revision_id

revision_sources
    revision_id, observation_id, source_kind, created_at

tag_hierarchies
    id, public_id, name, created_at, tombstoned_at?

tags
    id, public_id, hierarchy_id, parent_tag_id?, local_name,
    created_at, tombstoned_at?

blob_tag_assignments
    blob_id, hierarchy_id, tag_id, assigned_at

lineage_tag_assignments
    lineage_id, hierarchy_id, tag_id, assigned_at

revision_tag_assignments
    revision_id, hierarchy_id, tag_id, assigned_at

semantic_operations
    id, public_id, operation_kind, actor, created_at

semantic_changes
    operation_id, change_order, entity_kind, entity_reference,
    before_value?, after_value?

link_statements
    id, public_id, manifest_id, semantic_operation_id, submitter_name,
    submitter_version, submitter_configuration, is_bundle, base_components?,
    observed_since_publication?, evaluated_publication_sequence?,
    replaces_statement_id?, created_at, withdrawn_at?

manifest_links
    statement_id, ordinal, blob_id, record_ordinal?, entry_path?, role?,
    relative_path?, verified_by_read_id?, resolved_observation_id?
```

Assignment tables have one row per target and hierarchy, with composite
references ensuring the selected tag belongs to that hierarchy. Tag parentage
stays within a hierarchy. Enforce one root explicitly, including the null-parent
case, and prevent cycles. Tombstoned names and IDs remain reserved in the first
prototype to avoid ambiguous reuse.

Revision-parent pairs are unique. Transactional graph validation rejects
self-edges, cycles, and cross-Lineage edges. A source marked as an exact
manifestation must have location verification recognizing the revision's Blob at
that observation. Full recognition with an unconfirmed location can motivate an
operation but cannot establish an exact manifestation there. A looser motivating
source is labeled accordingly.

Semantic tables are current projections. Every mutation has an operation and
ordered changes in the same transaction. Before/after records use versioned
encodings and stable public references. Reject hard deletion of referenced
identities. The operation log records user decisions; it does not alter the
certainty of referenced filesystem evidence. An entity reference is a public ID
for an identity object, or the typed tuple of public IDs for a relationship such
as a tag assignment or revision edge.

A link statement and its links are inserted in one transaction with their
semantic operation, and they are immutable afterward except for `withdrawn_at`.
There are rows only for accepted statements; a rejected submission leaves no
statement. `replaces_statement_id` is unique, so a statement is superseded at
most once, and the statement it names must be current when the replacement is
accepted. `(statement_id, ordinal)` is unique. A link names a record only of its
statement's manifest. `relative_path` is present on every link of a bundle,
unique within the statement, and absent otherwise.

A link resolved from a path carries the read that verified its Blob and the
observation that showed the file unchanged, and its statement carries the
publication sequence it was evaluated against. A link submitted by Blob ID
carries neither. `base_components` records where a statement's paths were
resolved; for a bundle it is evidence of where the bundle was, not part of its
identity.

### 13.7 Rebuildable views and indexes

Current location trees, provisional directory instances, presence summaries,
ancestry queries, and tag ancestor closures may have cached projections. So may
current links, default extractor results, and bundle occurrence states. Cache
keys include the root binding, identity epoch, input publication boundary,
interpretation version, and all other query parameters. Semantic projections
also include their operation boundary. An epoch or binding change therefore
cannot reuse a cache keyed only by the last published scan. Caches retain
structured outcomes, reasons, and source references, including unresolved
relationships and coverage blockers. They are rebuildable and do not replace
retained scanner or semantic history. Unpersisted suggestions are not part of
the reconstruction inputs.

A cached location summary keeps typed references for latest metadata, latest
content assessment, last eligible verification, latest fingerprint comparison,
and location outcome. Listing coverage remains separate. Freshness queries also
include the as-of time and maintenance-policy version in their cache key. No
stored summary or score can create an exact association absent from its sources.

Plan indexes for observations by scan and parent, enumeration entries by attempt
and native name, native identity within an epoch, selected results, Blob
verification references, content digests, baseline policy lookup, semantic graph
edges, manifest records by manifest, and links by Blob. Indexes on chosen paths
of manifest data are optional. Cached root-relative paths are derived from
observation ancestry. Indexing choices must not change what evidence is retained
or which conclusions are valid.

## 14. Certainty and Simplicity Decisions

| Choice                                                     | Benefit                                                                                                | Explicit limitation                                                                                                                        |
| ---------------------------------------------------------- | ------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------ |
| Full hashing with stability checks                         | Strong exact-content recognition without managing file storage.                                        | Hash collision assumptions and undetected concurrent mutation remain; no filesystem snapshot is claimed.                                   |
| Occurrences derived from observations, with no File record | One place for each kind of fact: content on the Blob, placement in observations, meaning in a Lineage. | Nothing follows a file through edits and renames unless the user records a Lineage.                                                        |
| Plain content digests behind random Blob IDs               | Simple recognition with no secret to manage, and nothing content-derived in other pipelines.           | The catalog is as sensitive as the files; a digest that leaks is linkable to public hash lists.                                            |
| Separate content and location outcomes                     | Recognized bytes survive a failed location check without a false presence claim.                       | Knowing a Blob does not establish where it is currently located.                                                                           |
| Shared versioned interpretation                            | Advisors and frontends receive reproducible relationships, coverage, and reasons.                      | Consumers must preserve qualified results even when simplifying their presentation.                                                        |
| Root-relative locations plus native identity evidence      | Backup-friendly recovery and useful local replacement detection.                                       | Neither path nor inode is permanent object identity.                                                                                       |
| Conservative directory continuity within an epoch          | Prevents obvious stale-child attachment after replacement.                                             | Indistinguishable inode reuse remains possible; a new filesystem context, rename, or uncertain continuity requires fresh listing coverage. |
| Fixed sampled ranges                                       | Repeatable, inexpensive checks of expected Blob presence.                                              | Changes outside sampled regions may be missed indefinitely.                                                                                |
| Verification of selected files                             | Resolves uncertainty without rescanning unrelated content.                                             | Does not refresh directory listings or prove subtree-wide absence.                                                                         |
| Per-result coverage with atomic publication                | Useful siblings survive partial failures; readers see one database publication boundary.               | The filesystem observations still span different times.                                                                                    |
| Exclusive scan sessions and sequential attempts            | Simple transitions in scan/event order, with crash recovery.                                           | All other database users wait; external filesystem changes still require handle checks.                                                    |
| Inventory without content reads                            | Refreshes hierarchy and metadata at a cost driven principally by entry count.                          | Does not establish Blob identity or renew content evidence.                                                                                |
| Verification freshness with a finite horizon               | Prioritizes reads; matching Checks can defer work within a configurable two-year horizon.              | A maintenance score is not a probability, and cannot conceal contradictions or replace verification.                                       |
| Transient suggestions with durable accepted actions        | Users retain control and accepted decisions retain their reviewed evidence.                            | A persistent proposal inbox, dismissal, or deferral is an optional frontend workflow.                                                      |
| Advisors separate from extractors                          | Suggestions need no file access; derived metadata needs no heuristics.                                 | Derived metadata exists only for Blobs whose bytes an extractor has read.                                                                  |
| Extractor output as a function of the bytes                | A result holds wherever the content is found, and a rerun can be checked against it.                   | Anything contextual needs a link statement; a nondeterministic extractor shows up as a conflict.                                           |
| Plain manifest files with a mandatory schema               | Nothing is added to the managed tree, and every record of a kind has the same shape.                   | A schema must be written first; other formats need a converter.                                                                            |
| Link statements submitted as commands                      | Links are reviewable, replaceable as a whole, and kept in history.                                     | A statement is all or nothing, and a path must be verified and unchanged before it can be linked.                                          |
| Bundles as declared tree identity                          | A frozen layout can be checked with the existing comparison report.                                    | Changes are detected, not prevented; the identity is declared by a user, never inferred.                                                   |
| Complete retained history                                  | Supports explanation, correction, and later review.                                                    | History growth is accepted and is not a reason to discard evidence.                                                                        |

The product should distinguish "verified during this read", "plausibly still
present", "ruled out at this location", "last known", and "unknown". None means
that the filesystem cannot change immediately afterward. Offer verification or
broader scanning when the user's next action needs stronger evidence.

## 15. Implementation Sequence and Acceptance Cases

Implement in this order:

1. Root binding, exclusive sessions, sequential attempts, Inventory and Verify,
   lossless observations, stable full reads, exact Blob admission, publication,
   and recovery. Include schema enforcement of attempt and scan closure, plus
   selected-file and bounded-directory targets. Build the read loop with the two
   extractor attachment points of section 3.4, even though no extractor ships
   yet. Establish the command acceptance layer and transactional witnesses when
   exposing scan-request submission; reuse this infrastructure for later
   semantic and binding operations.
2. Directory continuity and the ordered transition table for the best-known
   view. Exercise mixed operations, retries, and replacements. Implement the
   shared interpretation contract, per-entry dates, and coverage blockers.
3. Versioned sampling baselines, Check, and freshness-based suggestions to
   verify overdue or unresolved selections. Implement section 6.3's canonical
   encoding, integer budget, Floyd selection, sample token, and shared test
   vectors. Benchmark the provisional parameters. Add comparison of candidate
   contexts, exception reports, and reviewed adoption of new filesystem
   identities. Validate that sampled results never create exact associations or
   silently activate a binding.
4. Lineages, revision graphs, scoped tags, semantic change history, and reviewed
   advisor proposals with provenance attached to accepted user actions.
5. Extractor identities, runs, and derived metadata, with the built-in JSON
   extractors. Registered schemas and validated manifests. Link statements with
   path resolution, replacement, and withdrawal. Bundles and their comparison
   report. This step depends on the read loop of step 1, the occurrences of step
   2, the comparison report of step 3, and the semantic operations of step 4.

Required acceptance cases include:

- Inventory refreshes hierarchy without reading file contents, advancing a
  verification date, or creating a Blob association. Unvisited and inaccessible
  subtrees remain explicit gaps.
- Check and Verify include the requested metadata work. Check reads 52 blocks
  (832 KiB) for a 1 MiB file under the default policy when a compatible baseline
  exists. Its 64 KiB plan covers all four blocks and runs Verify. Full
  recognition requires that full procedure; sampled matches cannot establish
  exact identity.
- Budget rounding accounts for a short final block and is deterministic across
  anchor/doubling boundaries, with no floating-point or overflow differences.
  Floyd selection gives the required distinct interior blocks plus endpoints.
- Verify and Check use identical block framing and token computation; coalescing
  adjacent reads does not change the token. Empty and full-coverage plans have
  explicit behavior. Different policies cannot be compared or silently
  substituted.
- A sampled file with no compatible baseline remains unresolved without an
  implicit full read. Changing only path, timestamps, or filesystem identity
  leaves selection and token unchanged under the same policy.
- A conflicting baseline for the same recognized Blob and policy preserves the
  existing baseline, records an integrity failure, and admits no conflicting
  baseline. Sample bytes and per-block fingerprints never enter durable outputs.
- A scan holds exclusive database access through finalization; another scan,
  query, advisor, or semantic operation waits. Recovery retains
  short-transaction progress without exposing an unpublished run as the current
  view.
- Appending an entry to an enumeration with a terminal result is rejected, even
  before its scan finalizes. Retrying uses a new attempt; references from later
  child work remain valid without changing the completed listing.
- Finalization commits target results, selected inputs, outcome, and publication
  atomically. A later command cannot append evidence or selection rows to the
  closed scan or change its interpretation at the same input boundary.
- Failed and abandoned scans reject later evidence just as published scans do.
  Recovery closes only unfinished sessions, never reopens finalized work, and
  does not fabricate terminal measurements for unfinished attempts.
- A later Inventory neither hides a fingerprint mismatch nor turns deliberately
  unrequested content work into a failed read. Access failure is never a
  mismatch.
- A later successful retry can complete its requested work; a later failure
  remains visible alongside historical success. Event order, not publication
  time, orders measurements, and enumeration attempts never share entries.
- A matching Check cannot erase a known content contradiction or postpone the
  configurable two-year verification deadline indefinitely. A never-verified
  occurrence has no borrowed verification age; overdue does not mean absent.
- An unidentified occurrence cannot be tagged as a Blob or used for a revision.
  Existing semantic records survive lost locations, and Verify can recover the
  same Blob identity without moving tags onto unidentified replacement content.
- Identical files and later restored copies converge on one Blob while retaining
  separate location observations and stable public Blob IDs.
- A replacement directory at the same path does not acquire old descendants.
- If enumeration sees inode 100 and a later open sees inode 200, the read
  targets a fresh opened observation. No size, timestamp, Blob identity, sample
  result, or other property measured from inode 200 is attributed to the
  inode-100 sighting, including fields that were originally unknown.
- A fresh opened observation does not become a member of the earlier enumeration
  or alter its entries. The trigger link explains why the read was attempted.
- An open failure creates no opened observation or content association. Matching
  native identity after an in-place edit still uses fresh measurements and keeps
  the earlier observation unchanged.
- Verification of three selected files can have outcome `succeeded`; its limited
  scope is not a gap. An unreadable requested file instead gives the run outcome
  `finished_with_gaps`, without changing its operation or requested scope.
- A suspected directory rename produces a refresh suggestion and retains history
  without asserting descendant continuity.
- A complete listing with one unreadable file still establishes its name
  coverage and preserves verified siblings.
- Verify on a selected file updates that file's evidence without refreshing its
  parent listing or its siblings.
- Sample mismatch removes plausible presence of the compared Blob at that
  location and offers verification; sample agreement never advances the last
  verification.
- An edit confined to unsampled bytes demonstrates the stated fixed-sample blind
  spot rather than being treated as an implementation failure or exact match.
- An unstable enumeration retry never mixes the first attempt's entries into the
  selected listing.
- A crash before publication exposes no partial current-state update, retains
  diagnostics, and permits the next process to mark the session abandoned.
- Root disappearance or replacement does not mark the entire archive deleted.
- Compare a root within its filesystem context before resetting identity epochs;
  a changed root must not disable an otherwise valid replacement comparison.
- A candidate scan on another host uses root-relative paths and content
  evidence, counting no native-identity differences as discrepancies. Its
  publication does not activate that binding.
- A complete candidate report with 18 exceptions among 10,006 locations
  qualifies for the initial at-most-1%-and-20 recommendation after structural
  review. Unresolved expected files count as exceptions; incomplete coverage or
  an empty reference cannot produce a misleading low-discrepancy recommendation.
- Adoption records the reviewed report, thresholds, evidence, and limitations
  atomically with activation. Exceeding a recommendation threshold does not
  prohibit explicit adoption with the unresolved scope acknowledged.
- A root or context change between comparison and acceptance requires fresh
  review. Adoption preserves sample status and observation times and transfers
  no old inode equivalence or unscanned coverage.
- A reboot or remount of the same filesystem continues the identity epoch.
  Occurrences keep their verification ages and directories keep their listing
  coverage. A changed device number alone does not start an epoch; a different
  filesystem identity at the configured root does.
- A new epoch prevents inherited directory coverage while allowing historical
  comparison targets and subsequent exact Blob recognition.
- A changed epoch or unavailable native identity produces unresolved continuity,
  not a replacement finding. Historical contents remain available without
  becoming established contents of the new context.
- Object-continuity evidence for a suspected rename does not automatically
  establish descendant attachment or refresh listing coverage.
- A later read or listing failure cannot manufacture absence or hide uncertainty
  behind an earlier successful result.
- A full read that passes handle checks and then loses its location before the
  final check admits recognized content without verifying that location.
- A failed or unstable content read admits no Blob or baseline even if the final
  location check succeeds.
- A coverage query reports both its scoped evidence and unresolved files,
  directories, or relationships. Scanning a selected file does not resolve an
  unenumerated parent or incompatible ancestor chain.
- Rebuilding interpretation results from the same facts, parameters, and rule
  version reproduces the same relationships, reasons, and coverage blockers.
- An unchanged file listed by five scans in one identity epoch yields five
  observations and one occurrence. Two verified hard links yield two occurrences
  of one Blob. Replacing the file at a path ends one occurrence and begins
  another without touching the earlier Blob, its tags, or its derived metadata.
- The same bytes yield the same content digest, sample positions, and sample
  token in two catalogs, and unrelated Blob IDs. A restored catalog recognizes
  content with no secret. No content digest or sample token appears in a log,
  diagnostic, exported reference, URL, filename, or extractor input.
- Derived metadata attaches only to the Blob recognized by the read that
  supplied the extractor's bytes; a path association or sample match attaches
  nothing. A new extractor version adds a record and overwrites none.
- Generating suggestions reads no file content. An extractor produces no
  suggestion.
- Generating and displaying advisor suggestions performs no required database
  writes. Acceptance records the request or semantic operation and the reviewed
  proposal's provenance atomically, without requiring a persistent suggestion.
- A stale semantic suggestion is revalidated before execution, and an accepted
  operation retains its evidence and change history.
- Retrying a committed command with the same witness, client request ID, and
  payload returns the original result, including after restart or later catalog
  changes. It creates no second request, semantic operation, binding activation,
  or accepted-proposal provenance record.
- Reusing a witness with a different operation type, client request ID, or
  payload fails. Concurrent submissions consume the witness at most once.
  Rejection or rollback of a first attempt leaves no partial action and does not
  consume the witness; a crash after commit leaves both the action and its
  replayable result.
- Retrying a scan submission returns the existing queued request and does not
  rerun the scan. An intentional repeat uses a new witness and client request
  ID; repeated Blob content in a Lineage remains valid.

- Identical bytes read by the same extractor identity in two unrelated
  directories yield one result. A different output under the same identity
  records `extractor_output_conflict`, keeps the first result, and admits
  nothing from the conflicting run for that Blob.
- A failed or timed-out extractor leaves the content result intact. Extractor
  outputs from an incomplete or unstable read are discarded. A seeking extractor
  is skipped when a result for its identity exists. Check runs no extractor.
- Retracting a run removes its results from default views and deletes nothing. A
  newer extractor version adds results that become the default.
- Marking a file as a manifest adds no file to the managed tree and leaves the
  manifest file untouched. One failing record creates no manifest and leaves an
  ordinary Blob with a diagnostic. A JSON Lines file yields one record per line.
  A manifest in another format is rendered by a converter running as an
  extractor on the verified original.
- A link statement that names every file by Blob ID is accepted without
  consulting any filesystem evidence. A statement never reads the filesystem.
- A path line resolves only to a verified occurrence that later evidence leaves
  unchanged. A file whose later observation differs in size or timestamps is
  rejected as `modified_since_verification`; a replaced or never verified file
  is `occurrence_unidentified`. Verifying the path and resubmitting succeeds.
- With `observed_since`, a path whose latest observation is older is rejected as
  `observation_too_old`.
- One invalid or unresolved line rejects the whole statement and records
  nothing. The result lists every failing line with its reason.
- A statement without a submitter name, version, and configuration is rejected.
- Two statements about unrelated manifests that link the same Blob both remain
  current. Neither changes the other, and a file view lists both.
- A replacement supersedes the earlier statement as a whole in one transaction,
  and both remain stored. A withdrawn statement leaves current links and stays
  in history. A statement cannot be superseded twice.
- A linked manifest without `bundle` records no layout. A bundle records a
  unique relative path for every link.
- A bundle moved intact to another directory is `intact` when checked at the new
  base. One changed or missing expected path makes it `damaged`. Files the
  bundle does not list are counted as added and do not change its state.
- Filestore neither prevents a change inside a bundle nor interprets a Merkle
  root, checksum list, or signature found in a manifest.

Exercise these interpretation invariants with synthetic histories independently
of the filesystem scanner. Scanner integration cases then verify that actual
filesystem operations produce the expected raw evidence and independent content
and location outcomes. Keep both kinds of checks: sound raw facts alone do not
guarantee sound conclusions in consumers.

Remaining implementation work includes validating the provisional sampling
parameters, freezing shared protocol test vectors, supported filesystem
validation (including how filesystem identity is read), native metadata
encodings, traversal resource limits, SQLite DDL and migrations (including
closure enforcement), and the UI and score curve for verification freshness and
mixed evidence. For extractors and manifests it includes extractor discovery,
process isolation, and time limits, the pinned JSON Schema dialect, operational
size limits for records and manifests, and the text forms of public IDs and of
path components that are not valid UTF-8. Storage of large extractor outputs and
running an extractor outside a scan are not yet designed. Command-layer
implementation also specifies witness issuance and encoding, payload comparison,
and the application ledger under section 10.1's transaction and retry contract.
These must implement the evidence rules above; they are not permission to treat
unknown content as recognized or uncovered directories as empty.
