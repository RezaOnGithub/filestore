# Filestore: Design Questions and Decisions

This file tracks design issues and their resolutions in [DESIGN.md](DESIGN.md).
Work through the numbered issues in order, recording the decision here and
updating the relevant design sections when a decision is made. Directions
explicitly marked proposed are not yet accepted requirements.

Q1 through Q12 are resolved. Q13 and the detailed rules of Q14 are proposed and
await review. Q15 and Q16 are open. Proposals that are not yet part of the
design, including running an extractor outside a scan and storage of large
extractor outputs, are in [IDEAS.md](IDEAS.md). Root replacement and rebinding
must support backups restored off the original host. The catalog's logical
archive and Blob identities must survive a change of host, filesystem, path, or
inode numbers when the catalog is preserved.

The three scan operations are **Inventory** (names, hierarchy, and metadata),
**Verify** (exact Blob identity), and **Check** (sampled fingerprint comparison,
with Verify when the rounded plan covers the whole file). Operation, scope, and
run outcome are independent. Each can cover selected files, a directory, or the
root. Outcomes are `succeeded`, `finished_with_gaps`, `failed`, and `abandoned`.
A limited scope is not a gap. **Check** replaces "quick scan"; reserve **full
read** for reading one file's entire contents and avoid "full scan" and "partial
scan". **Directory instance** replaces "directory incarnation"; continuity
remains a qualified interpretation.

The model's four nouns are **Blob** (which bytes), **Location** (where),
**Observation** (what one scan saw), and **Occurrence** (a Blob at a Location as
far as the evidence shows). "File" means only the object on disk. **Content
digest** replaces "Match ID". **Advisor** replaces "analyzer"; **extractor** and
**derived metadata** replace "Blob analysis". A **manifest** is a file whose
content is rendered as validated records, a **link statement** ties it to the
Blobs it describes, and a **bundle** is a link statement that also fixes the
files' relative layout.

## Q1. Root replacement, filesystem context, and epoch ordering

**Status:** Resolved; decision incorporated into DESIGN.md.

**References:**
[Root bindings, comparison, and adoption](DESIGN.md#33-bindings-identity-epochs-and-restores),
[root configuration schema](DESIGN.md#131-root-configuration-and-scanning).

**Issue:** The observed root identity was part of the context used to decide
whether comparison was permitted. A changed root could start a new epoch before
replacement was checked, turning detectable replacement into incomparable
identity. The restore workflow also needed a way to assess a new filesystem
context without demanding that native identifiers survive an off-host backup.

**Decision:** Offer adoption of the newly observed filesystem identities after
Check against a fixed reference view. Separate the filesystem comparison context
from the root object. Where the context is comparable, check root replacement
before resetting epochs. Where it is not, continuity is unresolved. Neither case
automatically activates a new binding.

The agreed workflow is:

1. Record a candidate binding and fresh native observations. Freeze the catalog
   reference by binding, epoch, publication boundary, interpretation version,
   and scope.
2. Compare candidate and reference by root-relative location, entry type, and
   Check results. Inode differences are not discrepancies.
3. Classify expected files as verified match, sample match, changed, missing, or
   unresolved. Count added file locations separately, and retain directory and
   other structural differences.
4. Calculate the exception budget and present the counts and their evidence:

   ```text
   exceptions = changed + missing + unresolved + added
   compared_locations = expected_file_locations + added_file_locations
   exception_rate = exceptions / compared_locations
   ```

5. Offer the initial low-discrepancy recommendation with complete namespace
   coverage, at most 1% exceptions, and at most 20 exceptions. Unknown reference
   areas or incomplete candidate coverage cannot support a whole-root
   recommendation. With no useful expected files, the score is insufficient
   evidence rather than perfect agreement.
6. Show exceptions by top-level directory, affected expected bytes, and distinct
   matched Blobs, separating full recognition from sample agreement. An entirely
   missing top-level subtree or significant type change needs explicit review
   even if the aggregate score is small.
7. On acceptance, activate the candidate binding and record the reference,
   comparison, thresholds, exceptions, and reviewed limitations atomically.
   Recheck the candidate root/context and the previous active binding before
   activation. Changed relevant evidence needs fresh review.

The thresholds are configurable recommendation defaults, not confidence bounds.
The user can explicitly adopt outside them or with unresolved scope after
review. A report with 18 exceptions among 10,006 locations is approximately
0.18% and qualifies when coverage is complete and structural differences are
reviewed.

A candidate scan is published only in its own binding until adoption. Activation
preserves its fresh identity epoch and the original evidence times and
strengths. Sample matches do not become location verification, old inode
identities are not rewritten or declared equivalent, and unscanned coverage is
not inherited.

**Implementation consequence:** The active binding is selected by an immutable
activation record rather than the latest binding row. This permits candidate
scanning without silently changing the active archive view. Activation can also
be the durable result referenced by accepted-action provenance.

**Acceptance cases:** Restore onto another host, detect a local replacement
before changing epochs, retain the active view during candidate scanning,
include unresolved files in the exception budget, expose concentrated structural
differences, reject stale activation preconditions, and retain explicit adoption
with unresolved scope. A missing root remains a failure rather than an empty
candidate. Full recognition continues to recover existing Blob IDs independently
of inode changes.

## Q2. Attribution of a read to the object actually opened

**Status:** Resolved; decision incorporated into DESIGN.md.

**References:** [Read procedure](DESIGN.md#34-open-handles-and-scan-stability),
[measurement ownership](DESIGN.md#35-attributing-measurements-to-the-opened-object),
[file-read schema](DESIGN.md#133-file-reads-blobs-and-baselines).

**Issue:** Enumeration sees `/report.pdf` at inode 100. Another program replaces
it, and the scanner then opens inode 200, reads it stably, and confirms its
location. The read is useful, but none of inode 200's properties may be
attributed to the earlier inode-100 sighting.

**Decision:** Distinguish the **trigger observation** that prompted the work
from the **opened observation** measured directly from the handle. Every
successful file open for a read creates a fresh opened observation, including
when its native identity agrees with the trigger. This preserves the earlier
snapshot through in-place edits as well as replacements.

The original sighting remains immutable, including unknown fields. Newly
measured size, timestamps, recognized Blob, sample results, byte counts, and
location outcomes belong only to the opened object and its read attempt. The
trigger link records provenance; it cannot transfer any of those properties.

Compare native identities only in a comparable context. A changed identity
requires fresh observations and re-resolution of affected ancestor anchors.
Unavailable comparison leaves continuity unresolved. Neither prevents retaining
valid content recognition; location verification still needs its own checks. An
old Blob may remain an explicit sampling comparison target without asserting
continuity with its former occurrence.

Do not insert the opened observation into the earlier enumeration or rewrite its
entries. Apply the same ownership rule to directory opens and listing attempts.
Open failures retain their diagnostic and any trigger without inventing an
opened object or content evidence. Failure before any observation is recorded as
a target failure.

**Implementation consequence:** File-read attempts have separate
`trigger_observation_id` and `opened_observation_id` references. Directory
enumeration attempts likewise distinguish their trigger from the freshly opened
directory. Insert immutable attempts after resolving the open, with those
references fixed. Selected results refer to attempts and obtain any measured
object association through the opened reference. A valid read of inode 200 need
not be discarded or require a rescan of unrelated siblings.

**Acceptance cases:** Replacement between listing and opening retains both
sightings and attributes no property of inode 200 to inode 100. Unknown fields
in the original stay unknown. Matching identity after an in-place edit also
leaves the earlier snapshot unchanged. A failed open creates no fictitious
object. New observations never change membership of an earlier listing.

## Q3. Evidence ordering and per-location transitions

**Status:** Resolved; decision incorporated into DESIGN.md.

**References:**
[Scan operations](DESIGN.md#51-operation-and-scope-are-independent),
[transition rules](DESIGN.md#73-projection-rules),
[exclusive scans](DESIGN.md#91-one-scanner-explicit-attempts),
[freshness policy](DESIGN.md#64-verification-freshness-is-a-maintenance-policy).

**Issue:** "Latest usable" evidence was underspecified when a location had
metadata observations, verifications, fingerprint checks, and failed attempts
from different times. Selecting only successful results could hide later
contradictions, while merely displaying the latest scan would lose earlier
information outside its scope.

**Decision:** Reduce read load through three operations and ordinary ordered
state transitions. Continuous file tracking is not a prerequisite. Preserve
uncertainty and offer Verify when exact identity is needed. Tags and revisions
retain their established Blob references independently of uncertain placement.

A scan takes exclusive application access to the database. Other scans, readers,
advisor queries, and semantic operations wait. The prototype also executes
filesystem attempts sequentially, including retries, so scan sequence and
within-scan event order suffice. External filesystem changes remain possible;
keep handle stability checks, location checks, and Q2's measurement ownership.

Each location's derived summary retains the latest metadata, latest content
assessment, last eligible verification, fingerprint comparison target/result,
and location outcome. Directory listing coverage has its own time and scope.
Apply these rules:

- Inventory refreshes structure and metadata without reading contents. Content
  work intentionally not requested neither erases nor renews earlier evidence.
- Check records plausible agreement, mismatch, or unresolved content. A matching
  sample never establishes an unidentified occurrence's Blob or refreshes its
  verification date. Full verification during Check is exact evidence; Q5
  defines when the sampling plan upgrades to Verify.
- Verify establishes the recognized Blob and, with confirmed location, the
  verified occurrence. It can recover existing Blob IDs and their semantic data.
- Stable, location-confirmed size or sample mismatch excludes the compared Blob
  for that occurrence. Failed access or an unstable read means unresolved, not
  different. A later sample match cannot conceal a known content contradiction.
- Metadata and native-identity differences alone do not establish changed
  content. Replacement keeps the old observation historical. A restore can use
  Q1's reviewed adoption; a moved inode provides only a candidate relationship.
- A file-only operation updates that file, not parent or sibling coverage.
  Replacement between enumeration and opening creates a fresh observation within
  the same scan; it does not require another scan session.
- Publish all terminal attempts as ordered inputs, including failures. A later
  successful retry can resolve an earlier failure; a later failure cannot vanish
  behind an earlier success. Keep original read, metadata, and location times.
  Never merge entries from different listing attempts into complete coverage.

A file without an established exact Blob association must be verified before it
can be used for Blob tagging or revisions. Existing Blobs, Lineages, and
revisions remain usable even without current occurrences. None of the scan
outcomes transfers their semantic records to unidentified content.

**Freshness policy:** Show per-entry observation, verification, and comparison
ages. A configurable horizon, provisionally two years after verification, caps
any freshness score even with repeated matching Checks. Only verification resets
the horizon. Zero means overdue; it is not proof of absence or corruption. The
score curve belongs to a versioned maintenance policy, while contradictions and
failures remain explicit. Never use the score as permission to establish a Blob
identity or carry another object's properties forward.

**Implementation consequence:** Requests and sessions specify `inventory`,
`verify`, or `check`. Inventory needs no content-read rows unless obtaining an
explicit stable metadata-only size exclusion. Shared queries apply section 7.3's
transition table; advisors suggest actions from the resulting states. An
exclusive application lock spans each scan, with short SQLite transactions for
durability. General arbitration between overlapping filesystem workers is
outside the prototype.

**Acceptance cases:** Inventory performs no content reads and preserves old
verification dates. A Check mismatch remains visible after a later Inventory. An
I/O failure cannot become a mismatch. An individual Verify does not refresh
siblings. Restored metadata or moved inode identity cannot create exact Blob
associations. A successful retry resolves only its own work; a later failure
remains visible. Rebuilding the same ordered history yields the same states.
Matching Checks cannot postpone the two-year horizon indefinitely. Other
database users wait until the scan releases exclusive access.

## Q4. Closing attempts and scans against later appends

**Status:** Resolved; lifecycle rules agreed. Concrete schema enforcement is
implementation work.

**References:** [Closure rules](DESIGN.md#94-closing-attempts-and-scans),
[scan schema](DESIGN.md#131-root-configuration-and-scanning),
[enumeration schema](DESIGN.md#132-observations-and-enumeration-attempts).

**Issue:** Append-only records and atomic publication do not, on their own,
prevent new entries from being appended to an enumeration that already has a
terminal result, or new selection rows from being attached to a published scan.

Such appends could change the answer produced from the same publication
boundary, breaking the interpretation contract. Q3's exclusive access removes
concurrent writers, but a later sequential command or recovery path can still
mistakenly append to an old attempt unless that state is checked.

For example, an Inventory run publishes a complete listing containing `a` and
`b`. A later command appends `c` using that old enumeration ID. Reopening the
same published run now appears to report three names, even though the scanner
never observed that listing. This is a database lifecycle issue.

**Decision:** Close each attempt when its terminal result is written, regardless
of success or failure. Reject later entries or measurements for that attempt;
retries use new attempts. Finalize each scan's remaining target results,
diagnostics, selected inputs, outcome, and any publication atomically. After
commit, reject additions to the closed scan, including failed or abandoned scans
that were never published. Publication cannot be attached afterward.

Recovery may close an unfinished session as abandoned after obtaining exclusive
access. Its unfinished attempts remain historical unfinished records; no
measurements are fabricated, and no later execution can complete them. Already
finalized work remains unchanged.

References to closed evidence remain valid. A later child read may refer to an
entry in a completed parent listing without joining that listing. Finalization
can select a completed attempt, and later scans or user actions can reference
old reads and baselines. Closure restricts evidence ownership and membership,
not ordinary references to history.

**Implementation consequence:** Enforce these rules through validated database
writes with constraints or triggers as appropriate. Schema work must distinguish
record existence from permission to append, enforce unique terminal records, and
allow publication within the finalization transaction without permitting
post-finalization inserts. Keep the existing atomic Blob/result/baseline
admission rule. Choose concrete DDL and transaction ordering during
implementation; this decision does not require another lifecycle table.

**Acceptance cases:** A completed listing rejects extra entries before or after
scan finalization. A later sequential command cannot add evidence or selection
rows to a closed scan. Failed and abandoned scans are also closed. Finalization
is atomic, recovery cannot reopen completed work, and later references to old
evidence do not alter its original membership or interpretation.

## Q5. Sampling protocol and baseline consistency

**Status:** Resolved; version-1 procedure and parameterized defaults agreed. Q8
later removed the database secret, and the text below describes the procedure
without it. Implementation must freeze shared test vectors and validate read
costs.

**Reference:** [Sampling policy](DESIGN.md#63-sampling-policy), including its
canonical encoding, selection pseudocode, fingerprint framing, and consistency
rules.

**Issue:** Verify and Check needed a shared, reproducible sampling procedure.
The initially suggested few fixed blocks provided too little coverage, while
blanket verification of small files could spend unnecessary reads. Restores and
same-size files with uncertain timestamps need a configurable content check
without attributing identity from filesystem metadata.

**Decision:** Use 16 KiB blocks and an 80% coverage anchor at 1 MiB. Below the
anchor, target 80% of the file. Above it, add 128 KiB of sampled bytes per
started doubling band. All four parameters are configurable. Calculate budgets
with exact integer arithmetic, clamp to file size, and account for a short last
block when rounding to whole blocks. The defaults select 52 blocks (832 KiB,
81.25%) at 1 MiB, 132 blocks (2.0625 MiB) at 1 GiB, and 212 blocks (3.3125 MiB)
at 1 TiB. These are read-budget choices, not probabilities of content equality.

Always include the first and last blocks where present. Derive a deterministic
BLAKE3 output stream from a fixed context string, the canonical policy, and the
file size. Read little-endian 64-bit words; rejection sampling gives unbiased
bounded integers. Use the specified Floyd loop to choose the required distinct
interior blocks uniformly, then sort offsets. Fix the procedure rather than
relying on a generic sampling API's implementation choices. Empty and one-block
files have explicit plans.

Hash the policy, file size, selected-block count, and each ordered block's
offset, actual length, and bytes under a second fixed context string. Persist
the 32-byte aggregate baseline only after an accepted full read. Check uses the
same selection and framing; coalesced reads do not change the logical block
boundaries. No sample bytes or per-block fingerprints enter durable outputs.
Block positions are public; sample tokens stay inside the catalog.

There is no separate small-file verification threshold. Check invokes Verify
when the rounded plan already covers the whole file, including empty files.
Otherwise, a sampled file without a compatible baseline remains unresolved and
prompts Verify; it is not silently read in full. A stable size mismatch against
a comparison Blob needs no content read.

The exact policy encoding is six little-endian unsigned 64-bit fields:
`version`, `block_size`, `anchor_size`, `coverage_numerator`,
`coverage_denominator`, and `growth_bytes`. The fraction is reduced. Version 1
fixes the hash contexts, budget rules, random-word mapping, Floyd loop, and
token framing in DESIGN.md. Policy changes create separate baselines; algorithm
changes need a new version. Paths, inode identities, timestamps, Blob IDs, and
scan IDs do not influence selection or the token. Fixed samples retain blind
spots, and repeated Checks do not accumulate new coverage.

For the same recognized Blob and canonical policy, baseline tokens must agree. A
discrepancy is an integrity error: preserve the old baseline, retain a failed
outcome and diagnostic, and do not overwrite it or misreport a routine content
change. This differs from a normal Check mismatch against a valid baseline.

**Implementation consequence:** Remove the independent `small_file_threshold`
from the session schema. Store canonical policy bytes, validate the version and
policy uniqueness, and enforce baseline equality on reuse. Implement shared
selection and framing for Verify and Check. Keep exact test vectors for empty,
short, partial-block, boundary, high-coverage, rejection, and collision cases.
Benchmark physical I/O and elapsed time as well as requested bytes; the default
80% read budget is not a promise of 20% less device I/O.

**Acceptance cases:** Both operations reproduce positions and tokens under the
same policy. Restore metadata changes do not change them. The 1 MiB case remains
sampled, full-coverage plans actually verify, missing baselines do not trigger
unrequested full reads, incompatible policies cannot compare, and baseline
conflicts fail without overwriting evidence. Unselected-byte changes can still
escape Check, as explicitly intended by the sampling trade-off.

## Q6. Idempotent acceptance of user actions

**Status:** Resolved; database witnesses belong to the command acceptance layer,
with both a server-issued token and a client request ID. Protocol encoding and
application-ledger schema remain implementation work.

**References:**
[Command acceptance and database witnesses](DESIGN.md#101-command-acceptance-and-database-witnesses),
[accepted-action provenance](DESIGN.md#135-accepted-action-provenance).

**Issue:** A frontend can submit an accepted action, lose the response after the
database commits, and submit it again. Atomic action/provenance storage prevents
partial records but does not prevent two complete operations from being created.

**Decision:** The server issues a unique, single-use witness for an operation
type. The client adds a unique request ID and submits the witness, request ID,
and operation payload. Retain the client request ID as part of the contract; it
identifies this submission rather than the client itself. Retries preserve all
three; an intentional repeat uses a new witness and a new client request ID.

On first acceptance, use one database transaction to validate the witness,
operation type, payload, and action preconditions and commit the operation, its
applicable accepted-proposal provenance, and a witness-consumption record
containing the client request ID, accepted payload, and durable result.
Serialize consumption so the witness can commit at most one operation. Rejection
or rollback creates neither consumption nor a partial operation.

An identical retry returns the original recorded result; a different operation
type, client request ID, or payload under the same witness fails. Recognize a
committed retry before applying action preconditions again: later catalog
changes do not make the original acknowledgment stale. Keep consumption and
result records across restart. Reporting only "already used" would leave a
client uncertain whether the original operation succeeded.

For scans, acceptance commits the queued scan request, not the filesystem scan.
A retry returns the same request; subsequent execution and status remain
separate. Semantic operations and root-binding activations return their existing
durable results in the same way.

**Architectural boundary:** A generic command acceptance layer owns this ledger
within the same database and transaction as the operation. It handles direct
commands and accepted advisor proposals. The essential catalog schema gains no
witness/client-request fields or dependencies. Provenance still explains the
reviewed decision and is distinct from the command receipt. Token format,
issuance representation, versioned payload comparison, API encoding, and ledger
DDL belong to application implementation; a network service is not required.

Do not prevent duplicates by making `(lineage_id, blob_id)` unique: repeated
content within a Lineage is an intentional semantic capability. Idempotency
belongs to action submission, not to Blob or revision equality, and does not
require persisting every generated suggestion.

**Acceptance cases:** A response lost after commit is recovered by an identical
retry, including after restart, without duplicating a scan request, semantic
operation, binding activation, or applicable provenance. Changing the request
ID, type, or payload under a consumed witness fails; concurrent submissions
consume it at most once. Rollback leaves neither a partial operation nor a
consumption record for the attempted first acceptance. Intentional repeats use
new identities and remain permitted.

## Q7. What a file is

**Status:** Resolved; decision incorporated into DESIGN.md.

**References:** [Terms and the four nouns](DESIGN.md#2-terms-and-boundaries),
[projection rules](DESIGN.md#73-projection-rules).

**Issue:** The design recorded paths, native identities, and filesystem metadata
but never said what a File is. Its only statement was negative: no durable
scanner-assigned identity for a file. "Occurrence", "occupant", and "slot"
filled the gap without definitions, and an observation row looked like a File
record. Readers disagreed about whether "the file" meant content, a path, or an
object on disk.

**Decision:** Four nouns carry the model. A **Blob** is which bytes. A
**Location** is where. An **Observation** is what one scan saw at a Location at
one time. An **Occurrence** is a Blob at a Location as far as the evidence
shows: a derived grouping of file observations with compatible continuity, whose
content evidence is verified, plausible, ruled out, or unidentified. It is the
file counterpart of a directory instance and has no stored identity.

"File" means only the object on disk. There is no File record and no File ID.
Facts about content attach to the Blob, facts about placement stay in
observations, and meaning that should survive edits and renames belongs to a
Lineage.

**Implementation consequence:** No table is added. The per-Location state
computed by section 7.3 is the occurrence. "Occupant" is retired in favor of
"occurrence".

**Acceptance cases:** An unchanged file listed by five scans in one identity
epoch yields five observations and one occurrence. Two verified hard links yield
two occurrences of one Blob. Replacing the file at a path ends one occurrence
and begins another; the earlier Blob, its tags, and its derived metadata are
unaffected. An unidentified occurrence has no Blob and cannot be tagged.

## Q8. Privacy boundary and plain content digests

**Status:** Resolved; decision incorporated into DESIGN.md. Amends Q5.

**References:**
[Content identity](DESIGN.md#4-exact-content-identity-and-privacy),
[privacy boundary](DESIGN.md#41-privacy-boundary),
[sampling policy](DESIGN.md#63-sampling-policy).

**Issue:** Content was recognized by a Match ID keyed with a database secret,
and the ordinary digest was never stored. The secret lived in the same database
as the Match IDs. It therefore protected only against a leak of Match IDs
without the rest of the catalog, and Match IDs were never exposed. The catalog
also holds paths, sizes, and tags, and with derived metadata it holds thumbnails
and perceptual fingerprints, which reveal more than a digest does. The keyed
scheme cost key derivations, a rekeying migration, keyed test vectors, and
incompatibility with ordinary checksums, and it obscured where the boundary
actually lies.

**Decision:** State the boundary as two rules. The catalog is as sensitive as
the managed files; nothing makes a leaked catalog safe. Outside the catalog,
content is named only by its random Blob ID, whose purpose is to keep content
identity out of other pipelines.

Recognize content by a plain BLAKE3 digest, stored privately and unique per
Blob. Remove the database matching secret and the Match ID. Sample positions and
tokens use BLAKE3's key-derivation mode with fixed context strings as a
domain-separated hash. Positions become a public function of policy and size.
That matters only against an adversary, which the non-adversarial baseline
already excludes. Content digests and sample tokens are never logged, exported,
or handed to other programs.

**Implementation consequence:** `blobs.match_id` becomes `content_digest`, and
`database_metadata` loses the secret. Restoring a catalog no longer depends on
preserving a secret. Scheme version 1 was never implemented, so it is redefined
in place. Test vectors need no keys.

**Acceptance cases:** The same bytes yield the same content digest, sample
positions, and sample token in two catalogs, and unrelated Blob IDs. A restored
catalog recognizes content with no secret. No content digest or sample token
appears in a log, diagnostic, exported reference, URL, filename, or extractor
input.

## Q9. Advisors and extractors replace "analysis"

**Status:** Resolved; the split is agreed and incorporated into DESIGN.md. The
names Advisor and Extractor are proposed.

**References:** [Advisors](DESIGN.md#10-advisors-suggest-user-actions),
[derived metadata and extractors](DESIGN.md#42-derived-metadata-and-extractors),
[derived-metadata schema](DESIGN.md#134-extractor-runs-derived-metadata-and-manifests).

**Issue:** "Analyzer" originally meant a heuristic that helps day-to-day
operation by finding patterns in occurrences and suggesting actions. Metadata
generation, such as format detection and perceptual fingerprints, was later
filed under the same word as "Blob analysis", and both shared a schema section.
The two differ in input, output, and cost: one reads the catalog and proposes
actions, the other reads file content and produces data.

**Decision:** Retire "analysis" and "analyzer". An **advisor** reads the catalog
and proposes suggestions. It reads no file content and causes no write until a
user accepts. An **extractor** reads the bytes of a verified Blob and produces
**derived metadata** attached to that Blob. It suggests nothing. An advisor may
read derived metadata. Only extractors and scans access the managed tree.

**Implementation consequence:** `blob_analyses` becomes `blob_derived_metadata`
with extractor columns, in its own schema section. `action_provenance` uses
advisor columns. The `analysis` schema group splits into `metadata` and
`advice`. Q11 later settled extractor identity and execution; storage of large
outputs remains a proposal in IDEAS.md.

**Acceptance cases:** Generating suggestions reads no file content and performs
no required database write. An extractor produces no suggestion. Derived
metadata attaches only to the Blob recognized by the read that supplied the
extractor's bytes. A new extractor version adds a record and overwrites none.

## Q10. Identity epochs survive reboots

**Status:** Resolved; decision incorporated into DESIGN.md. Amends Q1. How the
filesystem identity is read is implementation validation.

**References:**
[Bindings, epochs, and restores](DESIGN.md#33-bindings-identity-epochs-and-restores),
[projection rules](DESIGN.md#73-projection-rules),
[examples](DESIGN.md#74-examples).

**Issue:** The comparison context included the machine's boot instance, mount
namespace, and mount identity, so every reboot started a new identity epoch.
Continuity across an epoch boundary is unresolved, so each reboot detached every
directory listing and ended every occurrence together with its verification age.
Filestore targets PCs, which reboot routinely, not an always-on NAS. "Last
eligible verification" was also undefined, which hid the problem.

**Decision:** The comparison context is the persistent identity of the
filesystem holding the root: its UUID and type, with a subvolume identifier
where the filesystem has them. Boot instance, mount namespace, mount ID, and
device numbers are excluded. An epoch continues across scanner restarts,
remounts, and reboots while that identity agrees. Within an epoch, native
identity is compared by inode number and generation; device numbers are compared
only within one scan, to detect traversal boundaries.

An occurrence never spans epochs. A verification is eligible for an occurrence
when it belongs to that occurrence and no later content evidence contradicts it.
A new epoch now arises only from a new candidate binding, a different or
unidentifiable filesystem, or an explicit restore. Those cases go through Q1's
comparison and adoption.

**Implementation consequence:** `identity_epochs.runtime_context` becomes
`filesystem_context`. Supported-filesystem validation must establish how the
identity is read from the open root handle and which filesystems report a stable
one. Linux offers a generic filesystem-UUID ioctl (`FS_IOC_GETFSUUID`). A
filesystem that reports no stable identity is outside the supported baseline.

**Acceptance cases:** A reboot or remount of the same filesystem continues the
epoch; occurrences keep their verification ages and directories keep their
listing coverage. A changed device number alone does not start an epoch. A
different filesystem identity at the configured root starts a new epoch and
leads to candidate comparison. A replaced root on the same filesystem remains
replacement evidence, as in Q1.

## Q11. Extractor identity, runs, and execution

**Status:** Resolved; decision incorporated into DESIGN.md. Storage of large
outputs and running an extractor outside a scan remain proposals in IDEAS.md.

**References:**
[Derived metadata and extractors](DESIGN.md#42-derived-metadata-and-extractors),
[read procedure](DESIGN.md#34-open-handles-and-scan-stability),
[schema](DESIGN.md#134-extractor-runs-derived-metadata-and-manifests).

**Issue:** Q9 separated extractors from advisors but did not say who an
extractor is or when it runs. Nothing told results apart when the same Blob is
reached from unrelated directories or by later runs. A run in one directory
could attach data to a Blob that already carried data from another run, with no
rule for whether the two agree, coexist, or conflict.

**Decision:** An extractor's output is a function of the bytes it read and of
its own identity. It must not depend on the file's name, Location, neighbors, or
the time, so a result holds wherever the content is found. Contextual statements
belong in link statements under Q13.

An extractor identifies itself by name, version, and configuration. A run is one
identity executing within one scan, and every output records its run and the
accepted full read that supplied its bytes. The catalog keeps one result per
Blob and identity. A rerun that reproduces it adds nothing. A different output
under the same identity is `extractor_output_conflict`: the existing result
stays, a diagnostic names both runs, and nothing from the conflicting run is
admitted for that Blob. A different version, configuration, or extractor adds
its own result alongside. Corrections are a fixed version run again, and a user
operation can retract a run from default views.

Extractors run only during the full reads of a Verify request that names them. A
streaming extractor is fed by the read loop. A seeking extractor runs after the
hash, on the same open handle, before the final handle comparison, and is
skipped when its result already exists. Outputs are provisional until the read
is accepted and are committed with the content result. A failed extractor leaves
the content result intact. Check runs no extractor.

**Implementation consequence:** The read loop has two attachment points and is
built with them in the first implementation step, before any extractor ships.
`scan_extractions` records what a request asked for, `extractor_runs` what ran,
and `blob_derived_metadata` references its run and read.

**Acceptance cases:** Identical bytes read by one identity in two unrelated
directories yield one result. A conflicting output is recorded and admits
nothing. A failed or timed-out extractor leaves the content result intact, and
outputs of an unaccepted read are discarded. Retraction removes results from
default views and deletes nothing.

## Q12. Manifests and schemas

**Status:** Resolved; decision incorporated into DESIGN.md.

**References:** [Manifests and schemas](DESIGN.md#121-manifests-and-schemas),
[schema](DESIGN.md#134-extractor-runs-derived-metadata-and-manifests).

**Issue:** Descriptive data about files, such as sidecars, exports, and package
or mirror listings, should be queryable in the catalog. Two earlier sketches
failed. Free-form JSON with hand-made associations and paths into the document
was too loose for the volume involved. A Filestore file format that wrapped the
data together with Blob IDs needed a second copy of every manifest and could not
be added to a directory whose layout must not change.

**Decision:** A manifest is a plain file, read where it sits, with no Filestore
envelope and no Blob IDs. An extractor renders its content as JSON records: the
built-in `json` and `jsonl` extractors for files that are already JSON, and a
user-supplied converter running as an extractor for any other format. Filestore
knows no manifest formats.

A schema is a Blob holding a JSON Schema, parsed by the built-in `json-schema`
extractor and registered by a user operation under a unique name and version.
Every manifest names a registered schema. Every record is validated, and
validation is all or nothing. A Verify request marks a file as a manifest by
naming the extractor, configuration, and schema, and that request is the record
of the user's decision. Records are stored one row each as SQLite JSONB.

**Implementation consequence:** `manifest_schemas`, `manifests`, and
`manifest_records` join the extractor tables. Section 1 of DESIGN.md now says
that the catalog holds data derived from file contents, including a copy of what
a manifest says. The JSON Schema dialect and operational size limits are
implementation work.

**Acceptance cases:** Marking a file as a manifest adds no file to the managed
tree and leaves the manifest untouched. One failing record creates no manifest
and leaves an ordinary Blob with a diagnostic. A JSON Lines file yields one
record per line. A manifest in another format is rendered by a converter on the
verified original. A manifest may describe another manifest with no special
case.

## Q13. Link statements

**Status:** Proposed. Submitting links as a statement, not a file, is agreed.
The specification in DESIGN.md section 12.2 answers the questions below and
awaits review.

**References:** [Link statements](DESIGN.md#122-link-statements),
[command acceptance](DESIGN.md#101-command-acceptance-and-database-witnesses),
[schema](DESIGN.md#136-semantic-identities-and-changes).

**Issue:** A manifest must be tied to the Blobs it describes without adding a
file to the tree. The mechanism needed answers to these questions. What happens
when a named path points to a file that may have been modified? Can a statement
name Blob IDs instead, and who is then responsible? How are links corrected
later? What happens when a Blob already carries links from an unrelated run or
directory? How does the submitting program identify itself?

**Proposed decision:**

- A statement is JSON Lines submitted as a command. It names one manifest by ID
  and its submitter by name, version, and configuration. Each line links one
  Blob, optionally to one record and one entry inside it, with an optional role.
- A line names its file by Blob ID or by path. A Blob ID is taken as stated, and
  the submitter is responsible for it. A path is resolved from evidence already
  in the catalog and resolves only to a verified occurrence that later evidence
  leaves unchanged. A file that may have been modified is rejected as
  `modified_since_verification`; the remedy is to Verify it and resubmit.
- A statement never reads the filesystem. A frontend that wants a fresh look
  runs Inventory or Verify first and passes that scan's publication sequence as
  `observed_since`.
- A statement is accepted or rejected as a whole, and a rejection lists every
  failing line.
- Links belong to their statement. Links from unrelated statements on one Blob
  coexist; none is merged or overridden.
- An accepted statement is immutable. A correction is a new statement that
  replaces the earlier one as a whole, and a statement can also be withdrawn.
  Both leave history intact.

**Points to confirm in review:** Rejecting a whole statement for one unresolved
line. Requiring the manifest's ID in the header, with path lookup left to
frontends. Leaving `role` uninterpreted. Allowing a replacement to name a
different manifest. `observed_since` as the way to demand fresh evidence,
instead of letting a statement trigger its own scan.

**Implementation consequence:** `link_statements` and `manifest_links` join the
semantic tables, and a statement is a semantic operation that passes through the
command acceptance layer. Path resolution uses the interpretation component's
occurrence state. The text forms of public IDs and of path components that are
not valid UTF-8 are implementation work.

**Acceptance cases:** A statement naming only Blob IDs is accepted without
consulting filesystem evidence. A path whose later observation differs in size
or timestamps is rejected, and succeeds after Verify. One unresolved line
records nothing. A statement without a submitter identity is rejected. Two
unrelated statements linking one Blob both remain current. A replacement
supersedes the earlier statement in one transaction, and both remain stored.

## Q14. Bundles

**Status:** Resolved in concept and name. The rules in DESIGN.md section 12.3
are proposed and await review with Q13.

**References:** [Bundles](DESIGN.md#123-bundles),
[comparison report](DESIGN.md#33-bindings-identity-epochs-and-restores).

**Issue:** Some files and their manifest form a unit whose directory layout must
not change: a mirror of a public directory, a tree bound by a Merkle root, a
software package. The design had no identity for a tree, by intent, and no way
to say that a layout is part of what must be preserved. Not every file with a
manifest belongs to such a unit.

**Decision:** A **bundle** is a link statement that also records a relative path
for every linked Blob. It is a fixed mapping from relative path to Blob,
together with its manifest. It is declared by a user, never inferred, and it is
optional: a manifest can be linked without one.

**Proposed rules:** A bundle is identified by its statement and defined by
content, so it can occur at one base directory, at several, or at none. Checking
a directory against it reuses the restore comparison, classifying each expected
path and counting unlisted files as added. The occurrence is `intact`,
`plausibly intact`, `damaged`, or `unresolved`. Filestore detects changes inside
a bundle and does not prevent them, and it does not interpret a Merkle root,
checksum list, or signature found in a manifest.

**Implementation consequence:** No table is added beyond Q13's. A scan request's
comparison reference may be a bundle and a base directory.

**Acceptance cases:** A linked manifest without `bundle` records no layout. A
bundle moved intact to another directory is `intact` at the new base. One
changed or missing expected path makes it `damaged`. Unlisted files are counted
as added and do not change the state.

## Q15. Scan request lifecycle

**Status:** Open.

**References:**
[Scan requests](DESIGN.md#51-operation-and-scope-are-independent),
[recovery](DESIGN.md#92-durable-staging-and-atomic-publication),
[command acceptance](DESIGN.md#101-command-acceptance-and-database-witnesses).

**Issue:** A scan request is durable and queued, and its execution is a separate
session. The design does not say what starts execution, whether a queued request
can be cancelled, or what becomes of a request whose session was abandoned.
Abandoned sessions are not resumed, but nothing says whether the request runs
again. `scan_sessions.request_id` has no stated uniqueness.

**Proposed direction:** A request has at most one session. The submitting
command executes it, or a separate run command does when requests were queued
behind another scan. A failed or abandoned session finishes its request; running
the work again is a new request with a new witness. A queued request with no
session can be cancelled by a user operation that records a terminal result.

## Q16. Process model and exclusive access

**Status:** Open.

**References:** [Exclusive access](DESIGN.md#91-one-scanner-explicit-attempts),
[command acceptance](DESIGN.md#101-command-acceptance-and-database-witnesses).

**Issue:** The design refers to a server and to frontends, and it requires a
process-lifetime lock and draining of other database users before a scan. It
never says whether the prototype is a command-line program, a long-running
process, or both, and it names no locking mechanism. The first implementation
step cannot start without this.

**Proposed direction:** One binary used as a command-line program. Each command
opens the catalog and takes an advisory lock on a lock file in the catalog
directory: shared for queries and semantic commands, exclusive for a scan. The
operating system releases the lock when the process exits, which gives the
process-lifetime guarantee. A long-running frontend is the same code taking the
shared lock per operation, not a separate architecture.

## Other implementation decisions already recorded

[The implementation plan](DESIGN.md#15-implementation-sequence-and-acceptance-cases)
also leaves shared protocol test vectors, validation of the provisional sampling
parameters, supported filesystem validation, native metadata encodings,
operational resource limits, concrete SQLite DDL and migrations (including Q4
closure enforcement), the interpretation component's query interface and
reason-code catalogue, extractor isolation and limits, the JSON Schema dialect,
the text forms of public IDs and paths, Q6's command protocol and
application-ledger representation, and the freshness score curve and
presentation of mixed evidence for implementation. These remain implementation
work; they should not be mistaken for new contradictions in the architecture.

Retain the settled boundaries through implementation: complete history
retention, explicit uncertainty, strict evidence attachment, independent content
and location outcomes, and transient advisor suggestions with durable accepted
actions. History growth is not a reason to weaken any of these contracts.
