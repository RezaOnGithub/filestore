-- Filestore prototype schema, version 1.
--
-- Groups:
--   plumbing   scans, observations, enumerations, reads, blobs, baselines
--   metadata   derived_metadata (extractor output)
--   projection occurrences (rebuildable from published plumbing)
--   semantic   tags, lineages, revisions, semantic_ops
--   app        action_provenance, command_ledger, meta
--
-- Evidence rows are immutable and can be added only while their scan is
-- running; enumeration entries only while the enumeration has no result.

CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) STRICT;

-- An identity epoch: a period in which native identities (inode numbers)
-- under the root may be compared. The active epoch is the latest row.
CREATE TABLE epochs (
    id          INTEGER PRIMARY KEY,
    public_id   TEXT NOT NULL UNIQUE,
    fs_context  TEXT NOT NULL,
    root_ino    INTEGER NOT NULL,
    reason      TEXT NOT NULL,
    created_at  INTEGER NOT NULL
) STRICT;

-- Interned root-relative locations (namespace slots, not files).
CREATE TABLE locations (
    id        INTEGER PRIMARY KEY,
    parent_id INTEGER REFERENCES locations(id),
    name      BLOB NOT NULL,
    path      BLOB NOT NULL UNIQUE,
    CHECK ((parent_id IS NULL) = (path = X''))
) STRICT;
CREATE UNIQUE INDEX locations_parent_name ON locations(parent_id, name);

CREATE TABLE action_provenance (
    id              INTEGER PRIMARY KEY,
    advisor         TEXT NOT NULL,
    advisor_version INTEGER NOT NULL,
    suggestion_id   TEXT NOT NULL,
    input_seq       INTEGER NOT NULL,
    proposal        TEXT NOT NULL,   -- JSON snapshot of the reviewed suggestion
    accepted_at     INTEGER NOT NULL
) STRICT;

-- A scan request and its single execution session.
CREATE TABLE scans (
    id              INTEGER PRIMARY KEY,   -- scan sequence
    public_id       TEXT NOT NULL UNIQUE,
    operation       TEXT NOT NULL CHECK (operation IN ('inventory', 'verify', 'check')),
    targets         TEXT NOT NULL,         -- JSON, normalized
    requested_at    INTEGER NOT NULL,
    provenance_id   INTEGER REFERENCES action_provenance(id),
    state           TEXT NOT NULL DEFAULT 'queued'
                    CHECK (state IN ('queued', 'running', 'closed')),
    epoch_id        INTEGER REFERENCES epochs(id),
    policy          TEXT,                  -- JSON, actual policy used
    scanner_version TEXT,
    started_at      INTEGER,
    outcome         TEXT CHECK (outcome IN
                    ('succeeded', 'finished_with_gaps', 'failed', 'abandoned', 'cancelled')),
    finished_at     INTEGER,
    summary         TEXT,                  -- JSON counts, target results, gaps
    error           TEXT,
    publication_seq INTEGER UNIQUE,
    CHECK ((state = 'closed') = (outcome IS NOT NULL)),
    CHECK (publication_seq IS NULL OR outcome IN ('succeeded', 'finished_with_gaps'))
) STRICT;

CREATE TRIGGER scans_closed_immutable BEFORE UPDATE ON scans
WHEN OLD.state = 'closed'
BEGIN SELECT RAISE(ABORT, 'scan is closed'); END;

CREATE TRIGGER scans_no_delete BEFORE DELETE ON scans
BEGIN SELECT RAISE(ABORT, 'scans are retained'); END;

CREATE TABLE observations (
    id             INTEGER PRIMARY KEY,
    scan_id        INTEGER NOT NULL REFERENCES scans(id),
    location_id    INTEGER NOT NULL REFERENCES locations(id),
    parent_obs_id  INTEGER REFERENCES observations(id),
    role           TEXT NOT NULL CHECK (role IN ('root', 'anchor', 'entry', 'opened')),
    enumeration_id INTEGER REFERENCES enumerations(id),
    error          TEXT,       -- metadata could not be obtained
    kind           TEXT CHECK (kind IN ('file', 'dir', 'symlink', 'other')),
    dev            INTEGER,
    ino            INTEGER,
    size           INTEGER,
    mtime_ns       INTEGER,
    ctime_ns       INTEGER,
    mode           INTEGER,
    nlink          INTEGER,
    boundary       INTEGER NOT NULL DEFAULT 0,   -- on another device; not entered
    observed_at    INTEGER NOT NULL,
    event_order    INTEGER NOT NULL,
    CHECK ((role = 'entry') = (enumeration_id IS NOT NULL))
) STRICT;
CREATE INDEX observations_scan ON observations(scan_id, event_order);
CREATE INDEX observations_location ON observations(location_id);
CREATE INDEX observations_enumeration ON observations(enumeration_id);

CREATE TABLE enumerations (
    id             INTEGER PRIMARY KEY,
    scan_id        INTEGER NOT NULL REFERENCES scans(id),
    location_id    INTEGER NOT NULL REFERENCES locations(id),
    trigger_obs_id INTEGER REFERENCES observations(id),
    dir_obs_id     INTEGER REFERENCES observations(id),  -- freshly opened directory
    attempt        INTEGER NOT NULL,
    started_at     INTEGER NOT NULL,
    start_order    INTEGER NOT NULL
) STRICT;
CREATE INDEX enumerations_scan ON enumerations(scan_id);
CREATE INDEX enumerations_location ON enumerations(location_id);

CREATE TABLE enumeration_results (
    enumeration_id   INTEGER PRIMARY KEY REFERENCES enumerations(id),
    listing          TEXT NOT NULL CHECK (listing IN ('complete', 'incomplete', 'failed')),
    stability        TEXT NOT NULL CHECK (stability IN ('stable', 'unstable', 'unknown')),
    reason           TEXT,
    entries          INTEGER NOT NULL,
    after_mtime_ns   INTEGER,
    after_ctime_ns   INTEGER,
    location_check   TEXT NOT NULL CHECK (location_check IN
                     ('confirmed', 'contradicted', 'unresolved', 'not_checked')),
    location_reason  TEXT,
    finished_at      INTEGER NOT NULL,
    end_order        INTEGER NOT NULL
) STRICT;

CREATE TABLE blobs (
    id                    INTEGER PRIMARY KEY,
    public_id             TEXT NOT NULL UNIQUE,
    digest                BLOB NOT NULL UNIQUE CHECK (length(digest) = 32),  -- BLAKE3; private
    size                  INTEGER NOT NULL CHECK (size >= 0),
    admitted_from_read_id INTEGER NOT NULL REFERENCES reads(id),
    created_at            INTEGER NOT NULL
) STRICT;

CREATE TABLE sampling_policies (
    id         INTEGER PRIMARY KEY,
    encoding   TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL
) STRICT;

CREATE TABLE reads (
    id               INTEGER PRIMARY KEY,
    scan_id          INTEGER NOT NULL REFERENCES scans(id),
    location_id      INTEGER NOT NULL REFERENCES locations(id),
    trigger_obs_id   INTEGER REFERENCES observations(id),
    opened_obs_id    INTEGER REFERENCES observations(id),
    attempt          INTEGER NOT NULL,
    method           TEXT NOT NULL CHECK (method IN ('full', 'sample', 'size')),
    compared_blob_id INTEGER REFERENCES blobs(id),
    compare_source   TEXT,
    policy_id        INTEGER REFERENCES sampling_policies(id),
    started_at       INTEGER NOT NULL,
    start_order      INTEGER NOT NULL
) STRICT;
CREATE INDEX reads_scan ON reads(scan_id);
CREATE INDEX reads_location ON reads(location_id);

CREATE TABLE read_results (
    read_id             INTEGER PRIMARY KEY REFERENCES reads(id),
    content             TEXT NOT NULL CHECK (content IN
                        ('full_recognition', 'sample_match', 'sample_mismatch', 'size_mismatch',
                         'no_baseline', 'unreadable', 'unstable', 'not_regular', 'integrity_error')),
    reason              TEXT,
    bytes_read          INTEGER NOT NULL,
    recognized_blob_id  INTEGER REFERENCES blobs(id),
    after_size          INTEGER,
    after_mtime_ns      INTEGER,
    after_ctime_ns      INTEGER,
    location_check      TEXT NOT NULL CHECK (location_check IN
                        ('confirmed', 'contradicted', 'unresolved', 'not_checked')),
    location_reason     TEXT,
    location_checked_at INTEGER,
    finished_at         INTEGER NOT NULL,
    end_order           INTEGER NOT NULL,
    CHECK ((content = 'full_recognition') = (recognized_blob_id IS NOT NULL))
) STRICT;
CREATE INDEX read_results_blob ON read_results(recognized_blob_id);

CREATE TABLE baselines (
    blob_id              INTEGER NOT NULL REFERENCES blobs(id),
    policy_id            INTEGER NOT NULL REFERENCES sampling_policies(id),
    token                BLOB NOT NULL CHECK (length(token) = 32),   -- private
    created_from_read_id INTEGER NOT NULL REFERENCES reads(id),
    created_at           INTEGER NOT NULL,
    PRIMARY KEY (blob_id, policy_id)
) STRICT;

CREATE TABLE diagnostics (
    id          INTEGER PRIMARY KEY,
    scan_id     INTEGER NOT NULL REFERENCES scans(id),
    location_id INTEGER REFERENCES locations(id),
    severity    TEXT NOT NULL CHECK (severity IN ('info', 'warning', 'error')),
    code        TEXT NOT NULL,
    message     TEXT NOT NULL,
    at          INTEGER NOT NULL,
    event_order INTEGER NOT NULL
) STRICT;
CREATE INDEX diagnostics_scan ON diagnostics(scan_id);

-- Extractor output: a function of a Blob's bytes, attached to the Blob.
CREATE TABLE derived_metadata (
    id         INTEGER PRIMARY KEY,
    blob_id    INTEGER NOT NULL REFERENCES blobs(id),
    extractor  TEXT NOT NULL,
    version    TEXT NOT NULL,
    config     TEXT NOT NULL,
    payload    TEXT NOT NULL,
    scan_id    INTEGER NOT NULL REFERENCES scans(id),
    read_id    INTEGER NOT NULL REFERENCES reads(id),
    created_at INTEGER NOT NULL,
    UNIQUE (blob_id, extractor, version, config)
) STRICT;

-- Evidence closure: rows owned by a scan may only be added while it runs.
CREATE TRIGGER observations_open BEFORE INSERT ON observations
WHEN (SELECT state FROM scans WHERE id = NEW.scan_id) IS NOT 'running'
BEGIN SELECT RAISE(ABORT, 'scan is not running'); END;

CREATE TRIGGER observations_enumeration_open BEFORE INSERT ON observations
WHEN NEW.enumeration_id IS NOT NULL
 AND EXISTS (SELECT 1 FROM enumeration_results WHERE enumeration_id = NEW.enumeration_id)
BEGIN SELECT RAISE(ABORT, 'enumeration is closed'); END;

CREATE TRIGGER enumerations_open BEFORE INSERT ON enumerations
WHEN (SELECT state FROM scans WHERE id = NEW.scan_id) IS NOT 'running'
BEGIN SELECT RAISE(ABORT, 'scan is not running'); END;

CREATE TRIGGER enumeration_results_open BEFORE INSERT ON enumeration_results
WHEN (SELECT s.state FROM scans s JOIN enumerations e ON e.scan_id = s.id
      WHERE e.id = NEW.enumeration_id) IS NOT 'running'
BEGIN SELECT RAISE(ABORT, 'scan is not running'); END;

CREATE TRIGGER reads_open BEFORE INSERT ON reads
WHEN (SELECT state FROM scans WHERE id = NEW.scan_id) IS NOT 'running'
BEGIN SELECT RAISE(ABORT, 'scan is not running'); END;

CREATE TRIGGER read_results_open BEFORE INSERT ON read_results
WHEN (SELECT s.state FROM scans s JOIN reads r ON r.scan_id = s.id
      WHERE r.id = NEW.read_id) IS NOT 'running'
BEGIN SELECT RAISE(ABORT, 'scan is not running'); END;

CREATE TRIGGER diagnostics_open BEFORE INSERT ON diagnostics
WHEN (SELECT state FROM scans WHERE id = NEW.scan_id) IS NOT 'running'
BEGIN SELECT RAISE(ABORT, 'scan is not running'); END;

CREATE TRIGGER derived_metadata_open BEFORE INSERT ON derived_metadata
WHEN (SELECT state FROM scans WHERE id = NEW.scan_id) IS NOT 'running'
BEGIN SELECT RAISE(ABORT, 'scan is not running'); END;

-- Immutability of evidence.
CREATE TRIGGER observations_immutable BEFORE UPDATE ON observations
BEGIN SELECT RAISE(ABORT, 'observations are immutable'); END;
CREATE TRIGGER enumerations_immutable BEFORE UPDATE ON enumerations
BEGIN SELECT RAISE(ABORT, 'enumerations are immutable'); END;
CREATE TRIGGER enumeration_results_immutable BEFORE UPDATE ON enumeration_results
BEGIN SELECT RAISE(ABORT, 'enumeration results are immutable'); END;
CREATE TRIGGER reads_immutable BEFORE UPDATE ON reads
BEGIN SELECT RAISE(ABORT, 'reads are immutable'); END;
CREATE TRIGGER read_results_immutable BEFORE UPDATE ON read_results
BEGIN SELECT RAISE(ABORT, 'read results are immutable'); END;
CREATE TRIGGER blobs_immutable BEFORE UPDATE ON blobs
BEGIN SELECT RAISE(ABORT, 'blobs are immutable'); END;
CREATE TRIGGER baselines_immutable BEFORE UPDATE ON baselines
BEGIN SELECT RAISE(ABORT, 'baselines are immutable'); END;
CREATE TRIGGER observations_no_delete BEFORE DELETE ON observations
BEGIN SELECT RAISE(ABORT, 'observations are retained'); END;
CREATE TRIGGER reads_no_delete BEFORE DELETE ON reads
BEGIN SELECT RAISE(ABORT, 'reads are retained'); END;
CREATE TRIGGER blobs_no_delete BEFORE DELETE ON blobs
BEGIN SELECT RAISE(ABORT, 'blobs are retained'); END;
CREATE TRIGGER enumerations_no_delete BEFORE DELETE ON enumerations
BEGIN SELECT RAISE(ABORT, 'enumerations are retained'); END;
CREATE TRIGGER enumeration_results_no_delete BEFORE DELETE ON enumeration_results
BEGIN SELECT RAISE(ABORT, 'enumeration results are retained'); END;
CREATE TRIGGER read_results_no_delete BEFORE DELETE ON read_results
BEGIN SELECT RAISE(ABORT, 'read results are retained'); END;
CREATE TRIGGER baselines_no_delete BEFORE DELETE ON baselines
BEGIN SELECT RAISE(ABORT, 'baselines are retained'); END;
CREATE TRIGGER diagnostics_immutable BEFORE UPDATE ON diagnostics
BEGIN SELECT RAISE(ABORT, 'diagnostics are immutable'); END;
CREATE TRIGGER diagnostics_no_delete BEFORE DELETE ON diagnostics
BEGIN SELECT RAISE(ABORT, 'diagnostics are retained'); END;
CREATE TRIGGER derived_metadata_immutable BEFORE UPDATE ON derived_metadata
BEGIN SELECT RAISE(ABORT, 'derived metadata is immutable'); END;
CREATE TRIGGER derived_metadata_no_delete BEFORE DELETE ON derived_metadata
BEGIN SELECT RAISE(ABORT, 'derived metadata is retained'); END;
-- Locations, epochs, and policies are referenced by evidence: rewriting a
-- location's path would re-attribute everything observed there.
CREATE TRIGGER locations_immutable BEFORE UPDATE ON locations
BEGIN SELECT RAISE(ABORT, 'locations are immutable'); END;
CREATE TRIGGER locations_no_delete BEFORE DELETE ON locations
BEGIN SELECT RAISE(ABORT, 'locations are retained'); END;
CREATE TRIGGER epochs_immutable BEFORE UPDATE ON epochs
BEGIN SELECT RAISE(ABORT, 'epochs are immutable'); END;
CREATE TRIGGER epochs_no_delete BEFORE DELETE ON epochs
BEGIN SELECT RAISE(ABORT, 'epochs are retained'); END;
CREATE TRIGGER sampling_policies_immutable BEFORE UPDATE ON sampling_policies
BEGIN SELECT RAISE(ABORT, 'sampling policies are immutable'); END;
CREATE TRIGGER sampling_policies_no_delete BEFORE DELETE ON sampling_policies
BEGIN SELECT RAISE(ABORT, 'sampling policies are retained'); END;

-- Projection: the best-known state of each occurrence, derived from published
-- scans in publication order. Rebuildable; see project.rs.
CREATE TABLE occurrences (
    id                INTEGER PRIMARY KEY,
    epoch_id          INTEGER NOT NULL REFERENCES epochs(id),
    location_id       INTEGER NOT NULL REFERENCES locations(id),
    parent_occ_id     INTEGER REFERENCES occurrences(id),
    kind              TEXT,           -- NULL when metadata was never obtained
    ino               INTEGER,
    first_obs_id      INTEGER NOT NULL,
    first_seq         INTEGER NOT NULL,
    first_at          INTEGER NOT NULL,
    last_obs_id       INTEGER NOT NULL,
    last_seq          INTEGER NOT NULL,
    last_at           INTEGER NOT NULL,
    size              INTEGER,
    mtime_ns          INTEGER,
    ctime_ns          INTEGER,
    mode              INTEGER,
    boundary          INTEGER NOT NULL DEFAULT 0,
    meta_issue        TEXT,
    ended_seq         INTEGER,
    ended_at          INTEGER,
    end_reason        TEXT,
    -- File content evidence. content_blob_id is the verified, plausible, or
    -- ruled-out Blob depending on content; ev_* is the metadata of the read
    -- that produced the latest content evidence.
    content           TEXT NOT NULL DEFAULT 'unidentified'
                      CHECK (content IN ('unidentified', 'verified', 'plausible', 'changed')),
    content_blob_id   INTEGER REFERENCES blobs(id),
    content_read_id   INTEGER,
    content_at        INTEGER,
    ev_size           INTEGER,
    ev_mtime_ns       INTEGER,
    ev_ctime_ns       INTEGER,
    verified_blob_id  INTEGER REFERENCES blobs(id),
    verified_read_id  INTEGER,
    verified_at       INTEGER,
    check_result      TEXT,
    check_blob_id     INTEGER,
    check_read_id     INTEGER,
    check_at          INTEGER,
    prev_blob_id      INTEGER,        -- last verified Blob at this location before this occurrence
    issue             TEXT,           -- latest unresolved failure
    issue_detail      TEXT,
    issue_at          INTEGER,
    issue_read_id     INTEGER,
    -- Directory listing coverage.
    listing_enum_id   INTEGER,
    listing_at        INTEGER,
    listing_seq       INTEGER,
    listing_mtime_ns  INTEGER,
    listing_ctime_ns  INTEGER,
    listing_entries   INTEGER,
    listing_stale     INTEGER NOT NULL DEFAULT 0,
    listing_issue     TEXT,
    listing_issue_at  INTEGER
) STRICT;
CREATE UNIQUE INDEX occurrences_current ON occurrences(epoch_id, location_id) WHERE ended_seq IS NULL;
-- Full (not partial) so foreign-key checks on parent_occ_id can use it.
CREATE INDEX occurrences_parent ON occurrences(parent_occ_id);
CREATE INDEX occurrences_location ON occurrences(location_id);
CREATE INDEX occurrences_ino ON occurrences(epoch_id, ino);
CREATE INDEX occurrences_content_blob ON occurrences(content_blob_id);
CREATE INDEX occurrences_verified_blob ON occurrences(verified_blob_id);

-- Semantic records. Current projections plus an append-only operation log.
CREATE TABLE semantic_ops (
    id            INTEGER PRIMARY KEY,
    public_id     TEXT NOT NULL UNIQUE,
    kind          TEXT NOT NULL,
    actor         TEXT NOT NULL,
    summary       TEXT NOT NULL,
    changes       TEXT NOT NULL,   -- JSON array of {entity, before, after}
    provenance_id INTEGER REFERENCES action_provenance(id),
    created_at    INTEGER NOT NULL
) STRICT;

CREATE TABLE tag_hierarchies (
    id           INTEGER PRIMARY KEY,
    public_id    TEXT NOT NULL UNIQUE,
    name         TEXT NOT NULL UNIQUE,
    created_at   INTEGER NOT NULL
) STRICT;

CREATE TABLE tags (
    id           INTEGER PRIMARY KEY,
    public_id    TEXT NOT NULL UNIQUE,
    hierarchy_id INTEGER NOT NULL REFERENCES tag_hierarchies(id),
    parent_id    INTEGER REFERENCES tags(id),
    name         TEXT NOT NULL,
    created_at   INTEGER NOT NULL
) STRICT;
CREATE UNIQUE INDEX tags_sibling_names ON tags(hierarchy_id, coalesce(parent_id, 0), name);

-- One selected tag per target and hierarchy (exclusive hierarchies).
CREATE TABLE tag_assignments (
    target_kind  TEXT NOT NULL CHECK (target_kind IN ('blob', 'lineage', 'revision')),
    target_id    INTEGER NOT NULL,
    hierarchy_id INTEGER NOT NULL REFERENCES tag_hierarchies(id),
    tag_id       INTEGER NOT NULL REFERENCES tags(id),
    assigned_at  INTEGER NOT NULL,
    PRIMARY KEY (target_kind, target_id, hierarchy_id)
) STRICT;
CREATE INDEX tag_assignments_tag ON tag_assignments(tag_id);

CREATE TABLE lineages (
    id            INTEGER PRIMARY KEY,
    public_id     TEXT NOT NULL UNIQUE,
    title         TEXT NOT NULL,
    created_at    INTEGER NOT NULL,
    tombstoned_at INTEGER
) STRICT;

CREATE TABLE revisions (
    id            INTEGER PRIMARY KEY,
    public_id     TEXT NOT NULL UNIQUE,
    lineage_id    INTEGER NOT NULL REFERENCES lineages(id),
    blob_id       INTEGER NOT NULL REFERENCES blobs(id),
    label         TEXT,
    created_at    INTEGER NOT NULL,
    tombstoned_at INTEGER
) STRICT;
CREATE INDEX revisions_lineage ON revisions(lineage_id);
CREATE INDEX revisions_blob ON revisions(blob_id);

CREATE TABLE revision_parents (
    revision_id INTEGER NOT NULL REFERENCES revisions(id),
    parent_id   INTEGER NOT NULL REFERENCES revisions(id),
    PRIMARY KEY (revision_id, parent_id),
    CHECK (revision_id != parent_id)
) STRICT;

CREATE TRIGGER revisions_blob_immutable BEFORE UPDATE OF blob_id, lineage_id ON revisions
BEGIN SELECT RAISE(ABORT, 'revision content identity is immutable'); END;

-- Idempotent command acceptance: a retried request returns the original result.
CREATE TABLE command_ledger (
    request_id   TEXT PRIMARY KEY,
    operation    TEXT NOT NULL,
    payload_hash TEXT NOT NULL,
    result       TEXT NOT NULL,
    created_at   INTEGER NOT NULL
) STRICT;
