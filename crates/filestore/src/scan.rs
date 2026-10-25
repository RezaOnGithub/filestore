//! The synchronous scanner: Inventory, Verify, and Check over a scope.
//!
//! A scan runs to completion on the caller's thread. Each directory listing
//! and each file read is a separate attempt, executed sequentially and
//! committed in its own short transaction; hashing happens outside
//! transactions. Finalization closes the scan and, when it is publishable,
//! applies its evidence to the occurrence projection in the same transaction.
//!
//! Measurement ownership: every successful open for a listing or a read gets
//! a fresh `opened` observation measured from the handle. The listing entry
//! that prompted the work is recorded as the trigger and never receives the
//! opened object's measurements.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::OwnedFd;
use std::path::Path;
use std::time::Instant;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::extract;
use crate::fsx::{self, Kind, Meta};
use crate::ids::{new_public_id, now_ms};
use crate::pathenc;
use crate::sample::{self, Plan, SamplePolicy, TokenHasher};
use crate::{Error, Result, catalog, project};

pub const SCANNER_VERSION: &str = concat!("filestore-scanner/", env!("CARGO_PKG_VERSION"));
/// One retry after an unstable attempt; permission failures are not retried.
pub const RETRY_LIMIT: u32 = 1;
pub const ROOT_LOCATION: i64 = 1;
const READ_BUFFER: usize = 1 << 20;
/// Consecutive attempts share one write transaction for at most this long;
/// each attempt's own atomic sections nest inside it as savepoints.
const BATCH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);
/// Full reads of files at least this large run with no transaction open.
const LARGE_READ: u64 = 8 << 20;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Inventory,
    Verify,
    Check,
}

impl Operation {
    pub fn as_str(self) -> &'static str {
        match self {
            Operation::Inventory => "inventory",
            Operation::Verify => "verify",
            Operation::Check => "check",
        }
    }

    pub fn parse(s: &str) -> Result<Operation> {
        match s {
            "inventory" => Ok(Operation::Inventory),
            "verify" => Ok(Operation::Verify),
            "check" => Ok(Operation::Check),
            other => Err(Error::Invalid(format!("unknown operation {other:?}"))),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    Dir,
    File,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// Root-relative path in the API encoding (see `pathenc`).
    pub path: String,
    pub kind: TargetKind,
    /// Directory targets only: descendant-directory levels to enter.
    /// `0` lists the directory and handles its immediate files; `None` is
    /// unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ScanRequest {
    pub operation: Operation,
    pub targets: Vec<Target>,
}

#[derive(Serialize, Clone, Debug)]
pub struct ScanRef {
    pub id: i64,
    pub public_id: String,
}

/// Validate, deduplicate, and drop targets covered by other targets.
pub fn normalize_targets(targets: &[Target]) -> Result<Vec<Target>> {
    let mut decoded: Vec<(Vec<u8>, Target)> = Vec::new();
    for t in targets {
        let bytes = pathenc::decode_relative(&t.path)?;
        match t.kind {
            TargetKind::File if bytes.is_empty() => {
                return Err(Error::Invalid("the root is a directory target".into()));
            }
            TargetKind::File if t.depth.is_some() => {
                return Err(Error::Invalid("file targets take no depth".into()));
            }
            _ => {}
        }
        decoded.push((
            bytes.clone(),
            Target {
                path: pathenc::encode(&bytes),
                kind: t.kind,
                depth: t.depth,
            },
        ));
    }
    decoded.sort_by(|a, b| a.0.cmp(&b.0));
    decoded.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
    let dirs: Vec<(Vec<u8>, Option<u32>)> = decoded
        .iter()
        .filter(|(_, t)| t.kind == TargetKind::Dir)
        .map(|(p, t)| (p.clone(), t.depth))
        .collect();
    let covered = |path: &[u8], t: &Target| {
        dirs.iter().any(|(dp, dd)| {
            if dp.as_slice() == path && t.kind == TargetKind::Dir && t.depth == *dd {
                return false; // itself
            }
            let below = if dp.is_empty() {
                Some(path)
            } else if path == dp.as_slice() {
                Some(&b""[..])
            } else if path.starts_with(dp) && path.get(dp.len()) == Some(&b'/') {
                Some(&path[dp.len() + 1..])
            } else {
                None
            };
            let Some(below) = below else { return false };
            let k = pathenc::components(below).len() as u64;
            match (t.kind, dd) {
                (TargetKind::File, _) if k == 0 => false,
                (_, None) => true,
                (TargetKind::File, Some(d)) => k >= 1 && k - 1 <= u64::from(*d),
                (TargetKind::Dir, Some(d)) => match t.depth {
                    Some(td) => k + u64::from(td) <= u64::from(*d),
                    None => false,
                },
            }
        })
    };
    Ok(decoded
        .iter()
        .filter(|(p, t)| !covered(p, t))
        .map(|(_, t)| t.clone())
        .collect())
}

/// Record a durable queued scan request.
pub fn submit(conn: &Connection, req: &ScanRequest, provenance_id: Option<i64>) -> Result<ScanRef> {
    if req.targets.is_empty() {
        return Err(Error::Invalid("a scan needs at least one target".into()));
    }
    let targets = normalize_targets(&req.targets)?;
    let public_id = new_public_id("scan");
    conn.execute(
        "INSERT INTO scans (public_id, operation, targets, requested_at, provenance_id)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            public_id,
            req.operation.as_str(),
            serde_json::to_string(&targets)?,
            now_ms(),
            provenance_id
        ],
    )?;
    Ok(ScanRef {
        id: conn.last_insert_rowid(),
        public_id,
    })
}

/// Close a queued request that never started.
pub fn cancel_queued(conn: &Connection, scan_id: i64) -> Result<bool> {
    let n = conn.execute(
        "UPDATE scans SET state = 'closed', outcome = 'cancelled', finished_at = ?2,
                error = 'cancelled before it started'
         WHERE id = ?1 AND state = 'queued'",
        params![scan_id, now_ms()],
    )?;
    Ok(n > 0)
}

pub fn queued_scans(conn: &Connection) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare("SELECT id FROM scans WHERE state = 'queued' ORDER BY id")?;
    let ids = stmt
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<i64>>>()?;
    Ok(ids)
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Progress {
    pub scan_id: i64,
    pub operation: Option<Operation>,
    pub started_at: i64,
    pub dirs_listed: u64,
    pub entries_seen: u64,
    pub files_read: u64,
    pub bytes_read: u64,
    pub problems: u64,
    pub current: String,
}

pub trait Hooks {
    fn progress(&self, _p: &Progress) {}
    fn cancelled(&self) -> bool {
        false
    }
}

pub struct NoHooks;
impl Hooks for NoHooks {}

#[derive(Debug, Clone, Serialize)]
pub struct RunOutcome {
    pub scan_id: i64,
    pub outcome: String,
    pub publication_seq: Option<i64>,
    pub error: Option<String>,
    pub summary: Value,
}

#[derive(Default, Serialize)]
struct Counts {
    dirs_listed: u64,
    listing_failures: u64,
    listing_retries: u64,
    entries: u64,
    files_verified: u64,
    blobs_new: u64,
    blobs_recognized: u64,
    sample_match: u64,
    sample_mismatch: u64,
    size_mismatch: u64,
    no_baseline: u64,
    unreadable: u64,
    unstable: u64,
    not_regular: u64,
    integrity_errors: u64,
    location_unconfirmed: u64,
    read_retries: u64,
    bytes_read: u64,
    boundaries: u64,
    symlinks: u64,
    other_types: u64,
}

#[derive(Serialize)]
struct TargetResult {
    path: String,
    kind: TargetKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    depth: Option<u32>,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

struct LocCheck {
    status: &'static str,
    reason: Option<String>,
    at: i64,
}

struct ListedEntry {
    loc: i64,
    path: Vec<u8>,
    obs: i64,
    kind: Option<Kind>,
    boundary: bool,
}

struct Listing {
    dir_obs: Option<i64>,
    /// The listed object was confirmed at this path afterwards.
    located: bool,
    entries: Vec<ListedEntry>,
    usable: bool,
    retryable: bool,
}

struct Stream {
    digest: Option<[u8; 32]>,
    token: Option<[u8; 32]>,
    bytes: u64,
    error: Option<String>,
    after: Option<Meta>,
    outputs: Vec<(&'static str, &'static str, String, Value)>,
}

/// Content outcome of one read attempt.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Content {
    FullRecognition,
    SampleMatch,
    SampleMismatch,
    SizeMismatch,
    NoBaseline,
    Unreadable,
    Unstable,
    NotRegular,
    IntegrityError,
}

impl Content {
    fn as_str(self) -> &'static str {
        match self {
            Content::FullRecognition => "full_recognition",
            Content::SampleMatch => "sample_match",
            Content::SampleMismatch => "sample_mismatch",
            Content::SizeMismatch => "size_mismatch",
            Content::NoBaseline => "no_baseline",
            Content::Unreadable => "unreadable",
            Content::Unstable => "unstable",
            Content::NotRegular => "not_regular",
            Content::IntegrityError => "integrity_error",
        }
    }
}

struct Comparison {
    blob: Option<(i64, u64)>,
    source: &'static str,
}

struct Run<'a> {
    conn: &'a Connection,
    hooks: &'a dyn Hooks,
    scan_id: i64,
    op: Operation,
    epoch_id: i64,
    root: OwnedFd,
    root_path: std::path::PathBuf,
    root_meta: Meta,
    root_obs: i64,
    order: i64,
    policy: SamplePolicy,
    policy_id: i64,
    progress: Progress,
    last_report: Instant,
    counts: Counts,
    gap_total: u64,
    gaps_by_code: BTreeMap<String, u64>,
    integrity_failure: bool,
    listed: HashMap<i64, Option<u32>>,
    files_done: HashSet<i64>,
    paths: HashMap<i64, Vec<u8>>,
    cancelled: bool,
    buf: Vec<u8>,
    batch: Option<Instant>,
}

/// Execute a queued scan to completion.
pub fn run(conn: &Connection, root_path: &Path, scan_id: i64, hooks: &dyn Hooks) -> Result<RunOutcome> {
    let (op, targets_json, state): (String, String, String) = conn
        .query_row(
            "SELECT operation, targets, state FROM scans WHERE id = ?1",
            [scan_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("scan {scan_id}")))?;
    if state != "queued" {
        return Err(Error::Conflict(format!("scan {scan_id} is {state}, not queued")));
    }
    let op = Operation::parse(&op)?;
    let targets: Vec<Target> = serde_json::from_str(&targets_json)?;
    let policy = SamplePolicy::default();
    let policy_id = ensure_policy(conn, &policy)?;
    let extractors: Vec<String> = extract::default_stream_extractors()
        .iter()
        .map(|e| format!("{}/{} {}", e.name(), e.version(), e.config()))
        .collect();
    let policy_json = json!({
        "retry_limit": RETRY_LIMIT,
        "sampling": if op == Operation::Inventory { Value::Null } else { policy.encoding().into() },
        "extractors": if op == Operation::Verify || op == Operation::Check { extractors.into() } else { Value::Null },
        "boundaries": "symlinks not followed; mounts not crossed",
    });
    let started = conn.execute(
        "UPDATE scans SET state = 'running', started_at = ?2, scanner_version = ?3, policy = ?4
         WHERE id = ?1 AND state = 'queued'",
        params![scan_id, now_ms(), SCANNER_VERSION, policy_json.to_string()],
    )?;
    if started == 0 {
        return Err(Error::Conflict(format!("scan {scan_id} is no longer queued")));
    }
    let mut progress = Progress {
        scan_id,
        operation: Some(op),
        started_at: now_ms(),
        ..Default::default()
    };
    hooks.progress(&progress);

    let root = match fsx::open_root(root_path) {
        Ok(fd) => fd,
        Err(e) => {
            let msg = format!("cannot open root {}: {}", root_path.display(), fsx::errno_name(&e));
            return fail_scan(conn, scan_id, "root_unavailable", &msg);
        }
    };
    let root_meta = match fsx::meta_of(&root) {
        Ok(m) => m,
        Err(e) => {
            let msg = format!("cannot measure root: {}", fsx::errno_name(&e));
            return fail_scan(conn, scan_id, "root_unavailable", &msg);
        }
    };
    let ctx = fsx::fs_context(&root);
    let epoch: Option<(i64, String, i64)> = conn
        .query_row(
            "SELECT id, fs_context, root_ino FROM epochs ORDER BY id DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let epoch_id = match epoch {
        None => create_epoch(conn, &ctx, root_meta.ino, "initial")?,
        Some((id, ref ectx, ino)) if fsx::same_context(ectx, &ctx) && ino == root_meta.ino as i64 => {
            catalog::delete_meta(conn, "pending_root_change")?;
            id
        }
        Some((_, ectx, ino)) => {
            let (code, msg) = if !fsx::same_context(&ectx, &ctx) {
                (
                    "filesystem_context_changed",
                    format!("the root is now on filesystem [{ctx}], not [{ectx}]"),
                )
            } else {
                (
                    "root_replaced",
                    format!(
                        "the root directory was replaced (inode {} instead of {ino})",
                        root_meta.ino
                    ),
                )
            };
            let pending = json!({
                "reason": code,
                "message": msg,
                "fs_context": ctx,
                "root_ino": root_meta.ino,
                "previous_fs_context": ectx,
                "previous_root_ino": ino,
                "detected_at": now_ms(),
                "scan_id": scan_id,
            });
            catalog::set_meta(conn, "pending_root_change", &pending.to_string())?;
            return fail_scan(conn, scan_id, code, &msg);
        }
    };
    conn.execute("UPDATE scans SET epoch_id = ?2 WHERE id = ?1", params![scan_id, epoch_id])?;

    let mut run = Run {
        conn,
        hooks,
        scan_id,
        op,
        epoch_id,
        root,
        root_path: root_path.to_path_buf(),
        root_meta,
        root_obs: 0,
        order: 0,
        policy,
        policy_id,
        progress: std::mem::take(&mut progress),
        last_report: Instant::now(),
        counts: Counts::default(),
        gap_total: 0,
        gaps_by_code: BTreeMap::new(),
        integrity_failure: false,
        listed: HashMap::new(),
        files_done: HashSet::new(),
        paths: HashMap::from([(ROOT_LOCATION, Vec::new())]),
        cancelled: false,
        buf: vec![0u8; READ_BUFFER],
        batch: None,
    };
    let executed = run.execute(&targets, &root_meta).and_then(|r| {
        run.commit_batch()?;
        Ok(r)
    });
    match executed {
        Ok(results) => run.finish(results),
        Err(e) => {
            run.rollback_batch();
            let msg = format!("scanner error: {e}");
            fail_scan(conn, scan_id, "scanner_error", &msg)
        }
    }
}

fn ensure_policy(conn: &Connection, policy: &SamplePolicy) -> Result<i64> {
    conn.execute(
        "INSERT INTO sampling_policies (encoding, created_at) VALUES (?1, ?2)
         ON CONFLICT (encoding) DO NOTHING",
        params![policy.encoding(), now_ms()],
    )?;
    Ok(conn.query_row(
        "SELECT id FROM sampling_policies WHERE encoding = ?1",
        [policy.encoding()],
        |r| r.get(0),
    )?)
}

fn create_epoch(conn: &Connection, ctx: &str, root_ino: u64, reason: &str) -> Result<i64> {
    conn.execute(
        "INSERT INTO epochs (public_id, fs_context, root_ino, reason, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![new_public_id("epoch"), ctx, root_ino as i64, reason, now_ms()],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Adopt the root's current filesystem identity as a new identity epoch.
/// Earlier observations remain as history of the previous epoch.
pub fn adopt_root_identity(conn: &Connection, root_path: &Path) -> Result<i64> {
    let root = fsx::open_root(root_path)
        .map_err(|e| Error::io(format!("opening root {}", root_path.display()), e))?;
    let meta = fsx::meta_of(&root).map_err(|e| Error::io("measuring root", e))?;
    let ctx = fsx::fs_context(&root);
    let current: Option<(String, i64)> = conn
        .query_row(
            "SELECT fs_context, root_ino FROM epochs ORDER BY id DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if current.as_ref().is_some_and(|(c, ino)| fsx::same_context(c, &ctx) && *ino == meta.ino as i64) {
        catalog::delete_meta(conn, "pending_root_change")?;
        return Err(Error::Conflict(
            "the root's identity matches the active epoch; nothing to adopt".into(),
        ));
    }
    let tx = crate::catalog::atomic(conn)?;
    let id = create_epoch(&tx, &ctx, meta.ino, "adopted after root identity change")?;
    catalog::delete_meta(&tx, "pending_root_change")?;
    tx.commit()?;
    Ok(id)
}

/// Close a scan whose execution was interrupted by a crash of the scanning
/// code (a panic) rather than a recorded failure. Any open write transaction
/// is rolled back first; work committed earlier stays as unpublished history.
pub fn abort_after_panic(conn: &Connection, scan_id: i64) {
    if !conn.is_autocommit() {
        let _ = conn.execute_batch("ROLLBACK");
    }
    let _ = conn.execute(
        "UPDATE scans SET state = 'closed', outcome = 'failed', finished_at = ?2,
                error = 'the scanner crashed (internal error); nothing was published'
         WHERE id = ?1 AND state = 'running'",
        params![scan_id, now_ms()],
    );
}

/// Record a fatal failure and close the scan unpublished.
fn fail_scan(conn: &Connection, scan_id: i64, code: &str, message: &str) -> Result<RunOutcome> {
    let now = now_ms();
    // The diagnostic is best-effort: the failure may itself be a database error.
    let _ = conn.execute(
        "INSERT INTO diagnostics (scan_id, location_id, severity, code, message, at, event_order)
         VALUES (?1, NULL, 'error', ?2, ?3, ?4, 0)",
        params![scan_id, code, message, now],
    );
    let summary = json!({"fatal": code});
    conn.execute(
        "UPDATE scans SET state = 'closed', outcome = 'failed', finished_at = ?2, summary = ?3,
                error = ?4
         WHERE id = ?1 AND state = 'running'",
        params![scan_id, now, summary.to_string(), message],
    )?;
    Ok(RunOutcome {
        scan_id,
        outcome: "failed".into(),
        publication_seq: None,
        error: Some(message.to_string()),
        summary,
    })
}

impl Run<'_> {
    /// Called at attempt boundaries: open a write transaction, or commit and
    /// reopen it once it has been open for `BATCH_INTERVAL`. Committing per
    /// attempt costs dozens of WAL page writes per small file.
    fn batch(&mut self) -> Result<()> {
        match self.batch {
            Some(t) if t.elapsed() < BATCH_INTERVAL => {}
            Some(_) => {
                self.conn.execute_batch("COMMIT; BEGIN")?;
                self.batch = Some(Instant::now());
            }
            None => {
                self.conn.execute_batch("BEGIN")?;
                self.batch = Some(Instant::now());
            }
        }
        Ok(())
    }

    fn commit_batch(&mut self) -> Result<()> {
        if self.batch.take().is_some() {
            self.conn.execute_batch("COMMIT")?;
        }
        Ok(())
    }

    fn rollback_batch(&mut self) {
        if self.batch.take().is_some() {
            let _ = self.conn.execute_batch("ROLLBACK");
        }
    }

    fn next_order(&mut self) -> i64 {
        self.order += 1;
        self.order
    }

    fn report(&mut self, force: bool) {
        if force || self.last_report.elapsed().as_millis() >= 100 {
            self.progress.bytes_read = self.counts.bytes_read;
            self.hooks.progress(&self.progress);
            self.last_report = Instant::now();
        }
    }

    fn check_cancel(&mut self) -> bool {
        if !self.cancelled && self.hooks.cancelled() {
            self.cancelled = true;
        }
        self.cancelled
    }

    fn set_current(&mut self, rel: &[u8]) {
        self.progress.current = pathenc::display(rel);
        self.report(false);
    }

    fn execute(&mut self, targets: &[Target], root_meta: &Meta) -> Result<Vec<TargetResult>> {
        self.root_obs = self.insert_obs(ROOT_LOCATION, None, "root", None, Ok(root_meta), false, now_ms())?;
        let mut results = Vec::new();
        for t in targets {
            if self.check_cancel() {
                results.push(TargetResult {
                    path: t.path.clone(),
                    kind: t.kind,
                    depth: t.depth,
                    status: "not_started",
                    reason: Some("scan cancelled".into()),
                });
                continue;
            }
            let before = self.gap_total;
            let reason = self.target(t)?;
            let status = match (&reason, self.gap_total > before) {
                (Some(_), _) => "failed",
                (None, true) => "finished_with_gaps",
                (None, false) => "done",
            };
            results.push(TargetResult {
                path: t.path.clone(),
                kind: t.kind,
                depth: t.depth,
                status,
                reason,
            });
        }
        if self.cancelled {
            self.gap(None, "cancelled", "scan cancelled by the user; remaining requested work was not performed")?;
        }
        Ok(results)
    }

    fn finish(mut self, targets: Vec<TargetResult>) -> Result<RunOutcome> {
        self.report(true);
        // An unexpected replacement of the root during the scan publishes
        // nothing: every location check was relative to the old root handle.
        let now_at_root = fsx::open_root(&self.root_path).and_then(|fd| fsx::meta_of(&fd));
        match now_at_root {
            Ok(m) if m.dev == self.root_meta.dev && m.ino == self.root_meta.ino => {}
            Ok(m) => {
                let msg = format!(
                    "the root directory was replaced during the scan (inode {} instead of {}); nothing is published",
                    m.ino, self.root_meta.ino
                );
                return fail_scan(self.conn, self.scan_id, "root_replaced", &msg);
            }
            Err(e) => {
                let msg = format!("the root became unavailable during the scan ({}); nothing is published", fsx::errno_name(&e));
                return fail_scan(self.conn, self.scan_id, "root_unavailable", &msg);
            }
        }
        let outcome = if self.integrity_failure {
            "failed"
        } else if self.gap_total > 0 {
            "finished_with_gaps"
        } else {
            "succeeded"
        };
        let summary = json!({
            "counts": self.counts,
            "gaps": self.gap_total,
            "gaps_by_code": self.gaps_by_code,
            "targets": targets,
            "cancelled": self.cancelled,
        });
        let error = self
            .integrity_failure
            .then(|| "integrity error: recognized content conflicts with the catalog".to_string());
        let conn = self.conn;
        let tx = crate::catalog::atomic(conn)?;
        let seq: Option<i64> = if outcome == "failed" {
            None
        } else {
            Some(tx.query_row(
                "SELECT coalesce(max(publication_seq), 0) + 1 FROM scans",
                [],
                |r| r.get(0),
            )?)
        };
        let closed = tx.execute(
            "UPDATE scans SET state = 'closed', outcome = ?2, finished_at = ?3, summary = ?4,
                    error = ?5, publication_seq = ?6
             WHERE id = ?1 AND state = 'running'",
            params![self.scan_id, outcome, now_ms(), summary.to_string(), error, seq],
        )?;
        if closed != 1 {
            return Err(Error::Conflict(format!("scan {} was not running at finalization", self.scan_id)));
        }
        if let Some(seq) = seq {
            project::apply_scan(&tx, self.scan_id, seq)?;
        }
        tx.commit()?;
        Ok(RunOutcome {
            scan_id: self.scan_id,
            outcome: outcome.into(),
            publication_seq: seq,
            error,
            summary,
        })
    }

    // ----- bookkeeping -------------------------------------------------

    fn path_of(&mut self, loc: i64) -> Result<Vec<u8>> {
        if let Some(p) = self.paths.get(&loc) {
            return Ok(p.clone());
        }
        let p: Vec<u8> = self
            .conn
            .query_row("SELECT path FROM locations WHERE id = ?1", [loc], |r| r.get(0))?;
        self.paths.insert(loc, p.clone());
        Ok(p)
    }

    fn child_location(&mut self, parent: i64, name: &[u8]) -> Result<i64> {
        let found: Option<i64> = self
            .conn
            .prepare_cached("SELECT id FROM locations WHERE parent_id = ?1 AND name = ?2")?
            .query_row(params![parent, name], |r| r.get(0))
            .optional()?;
        if let Some(id) = found {
            return Ok(id);
        }
        let path = pathenc::join(&self.path_of(parent)?, name);
        self.conn
            .prepare_cached("INSERT INTO locations (parent_id, name, path) VALUES (?1, ?2, ?3)")?
            .execute(params![parent, name, path])?;
        let id = self.conn.last_insert_rowid();
        self.paths.insert(id, path);
        Ok(id)
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_obs(
        &mut self,
        loc: i64,
        parent: Option<i64>,
        role: &str,
        enumeration: Option<i64>,
        meta: std::result::Result<&Meta, &str>,
        boundary: bool,
        at: i64,
    ) -> Result<i64> {
        let order = self.next_order();
        let (error, m) = match meta {
            Ok(m) => (None, Some(m)),
            Err(e) => (Some(e), None),
        };
        self.conn
            .prepare_cached(
                "INSERT INTO observations
                   (scan_id, location_id, parent_obs_id, role, enumeration_id, error, kind, dev, ino,
                    size, mtime_ns, ctime_ns, mode, nlink, boundary, observed_at, event_order)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            )?
            .execute(params![
                self.scan_id,
                loc,
                parent,
                role,
                enumeration,
                error,
                m.map(|m| m.kind.as_str()),
                m.map(|m| m.dev as i64),
                m.map(|m| m.ino as i64),
                m.map(|m| m.size as i64),
                m.map(|m| m.mtime_ns),
                m.map(|m| m.ctime_ns),
                m.map(|m| m.mode),
                m.map(|m| m.nlink as i64),
                boundary,
                at,
                order
            ])?;
        Ok(self.conn.last_insert_rowid())
    }

    fn diag(&mut self, loc: Option<i64>, severity: &str, code: &str, message: &str) -> Result<()> {
        let order = self.next_order();
        self.conn
            .prepare_cached(
                "INSERT INTO diagnostics (scan_id, location_id, severity, code, message, at, event_order)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?
            .execute(params![self.scan_id, loc, severity, code, message, now_ms(), order])?;
        Ok(())
    }

    /// Requested work that did not complete: a diagnostic plus a gap count.
    fn gap(&mut self, loc: Option<i64>, code: &str, message: &str) -> Result<()> {
        self.gap_total += 1;
        *self.gaps_by_code.entry(code.to_string()).or_default() += 1;
        self.progress.problems += 1;
        self.diag(loc, "warning", code, message)
    }

    fn location_check(&mut self, rel: &[u8], expected: &Meta) -> LocCheck {
        let r = fsx::locate(&self.root, rel);
        let at = now_ms();
        let (status, reason) = match r {
            Ok(m) if m.dev == expected.dev && m.ino == expected.ino => ("confirmed", None),
            Ok(m) => (
                "contradicted",
                Some(format!("a different object is at this path (inode {})", m.ino)),
            ),
            Err(e) => match e.raw_os_error() {
                Some(libc::ENOENT) | Some(libc::ENOTDIR) => (
                    "contradicted",
                    Some(format!("the path no longer resolves ({})", fsx::errno_name(&e))),
                ),
                Some(libc::ELOOP) => ("contradicted", Some("a symlink is now in the path".into())),
                Some(libc::EXDEV) => ("contradicted", Some("a mount is now in the path".into())),
                _ => ("unresolved", Some(fsx::errno_name(&e))),
            },
        };
        LocCheck { status, reason, at }
    }

    // ----- targets and traversal ---------------------------------------

    /// Run one target. Returns a failure reason when the target itself could
    /// not be resolved or performed.
    fn target(&mut self, t: &Target) -> Result<Option<String>> {
        self.batch()?;
        let path = pathenc::decode_relative(&t.path)?;
        let comps = pathenc::components(&path);
        let ancestors = match t.kind {
            TargetKind::Dir => comps.len().saturating_sub(1),
            TargetKind::File => comps.len() - 1,
        };
        let mut loc = ROOT_LOCATION;
        let mut parent_obs = Some(self.root_obs);
        for i in 0..ancestors {
            loc = self.child_location(loc, comps[i])?;
            let prefix = comps[..=i].join(&b'/');
            match fsx::locate(&self.root, &prefix) {
                Ok(m) => {
                    let obs = self.insert_obs(loc, parent_obs, "anchor", None, Ok(&m), false, now_ms())?;
                    if m.kind != Kind::Dir {
                        let reason = format!("{} is a {}, not a directory", pathenc::display(&prefix), m.kind.as_str());
                        self.gap(Some(loc), "target_unresolved", &reason)?;
                        return Ok(Some(reason));
                    }
                    parent_obs = Some(obs);
                }
                Err(e) => {
                    let reason = format!(
                        "cannot resolve {}: {}",
                        pathenc::display(&prefix),
                        fsx::errno_name(&e)
                    );
                    self.gap(Some(loc), "target_unresolved", &reason)?;
                    return Ok(Some(reason));
                }
            }
        }
        match t.kind {
            TargetKind::Dir => {
                let (loc, parent_obs) = if comps.is_empty() {
                    (ROOT_LOCATION, None)
                } else {
                    (self.child_location(loc, comps[comps.len() - 1])?, parent_obs)
                };
                let listing = self.process_dir(loc, &path, parent_obs, None, t.depth)?;
                Ok(match listing {
                    Some(false) => Some("the directory could not be listed".into()),
                    _ => None,
                })
            }
            TargetKind::File => {
                let loc = self.child_location(loc, comps[comps.len() - 1])?;
                self.file_target(loc, &path, parent_obs)
            }
        }
    }

    fn file_target(&mut self, loc: i64, rel: &[u8], parent_obs: Option<i64>) -> Result<Option<String>> {
        if !self.files_done.insert(loc) {
            return Ok(None);
        }
        self.batch()?;
        match self.op {
            Operation::Inventory => match fsx::locate(&self.root, rel) {
                Ok(m) => {
                    self.insert_obs(loc, parent_obs, "anchor", None, Ok(&m), false, now_ms())?;
                    self.progress.entries_seen += 1;
                    Ok(None)
                }
                Err(e) => {
                    let reason = format!("cannot resolve {}: {}", pathenc::display(rel), fsx::errno_name(&e));
                    self.gap(Some(loc), "target_unresolved", &reason)?;
                    Ok(Some(reason))
                }
            },
            Operation::Verify => {
                self.verify_file(loc, rel, parent_obs, None)?;
                Ok(None)
            }
            Operation::Check => {
                self.check_file(loc, rel, parent_obs, None)?;
                Ok(None)
            }
        }
    }

    /// List a directory (with one retry if unstable) and apply the operation
    /// to its files and, within `depth`, its subdirectories. Returns whether
    /// the final listing was usable, or `None` if skipped.
    fn process_dir(
        &mut self,
        loc: i64,
        rel: &[u8],
        parent_obs: Option<i64>,
        trigger: Option<i64>,
        depth: Option<u32>,
    ) -> Result<Option<bool>> {
        if self.check_cancel() {
            return Ok(None);
        }
        if let Some(done) = self.listed.get(&loc) {
            let covered = match (done, depth) {
                (None, _) => true,
                (Some(_), None) => false,
                (Some(a), Some(b)) => *a >= b,
            };
            if covered {
                return Ok(None);
            }
        }
        self.listed.insert(loc, depth);
        let mut attempt = 1;
        let listing = loop {
            let l = self.enumerate(loc, rel, parent_obs, trigger, attempt)?;
            if l.retryable && attempt <= RETRY_LIMIT {
                attempt += 1;
                self.counts.listing_retries += 1;
                continue;
            }
            break l;
        };
        let Some(dir_obs) = listing.dir_obs else {
            return Ok(Some(false));
        };
        if !listing.located {
            // Work below would be anchored to an object no longer known to be
            // at this path; it could only attach to the wrong instance.
            let msg = format!(
                "entries of {} were not processed: the listed directory could not be confirmed at its path",
                display_dir(rel)
            );
            self.gap(Some(loc), "descendants_skipped", &msg)?;
            return Ok(Some(false));
        }
        let child_depth = match depth {
            None => Some(None),
            Some(0) => None,
            Some(d) => Some(Some(d - 1)),
        };
        let mut subdirs = Vec::new();
        for e in &listing.entries {
            if e.boundary {
                continue;
            }
            match e.kind {
                Some(Kind::File) => {
                    if self.check_cancel() {
                        break;
                    }
                    if self.files_done.insert(e.loc) {
                        match self.op {
                            Operation::Inventory => {}
                            Operation::Verify => self.verify_file(e.loc, &e.path, Some(dir_obs), Some(e.obs))?,
                            Operation::Check => self.check_file(e.loc, &e.path, Some(dir_obs), Some(e.obs))?,
                        }
                    }
                }
                Some(Kind::Dir) => subdirs.push((e.loc, e.path.clone(), e.obs)),
                _ => {}
            }
        }
        if let Some(cd) = child_depth {
            for (cloc, cpath, cobs) in subdirs {
                self.process_dir(cloc, &cpath, Some(dir_obs), Some(cobs), cd)?;
            }
        }
        Ok(Some(listing.usable))
    }

    fn enumerate(
        &mut self,
        loc: i64,
        rel: &[u8],
        parent_obs: Option<i64>,
        trigger: Option<i64>,
        attempt: u32,
    ) -> Result<Listing> {
        self.batch()?;
        let started_at = now_ms();
        let start_order = self.next_order();
        self.set_current(rel);
        let opened = fsx::open_dir(&self.root, rel).and_then(|fd| fsx::meta_of(&fd).map(|m| (fd, m)));
        let (fd, before) = match opened {
            Ok(x) => x,
            Err(e) => {
                let reason = format!("open failed: {}", fsx::errno_name(&e));
                let tx = crate::catalog::atomic(self.conn)?;
                tx.execute(
                    "INSERT INTO enumerations (scan_id, location_id, trigger_obs_id, dir_obs_id, attempt,
                                               started_at, start_order)
                     VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6)",
                    params![self.scan_id, loc, trigger, attempt, started_at, start_order],
                )?;
                let enum_id = tx.last_insert_rowid();
                let end_order = self.next_order();
                tx.execute(
                    "INSERT INTO enumeration_results (enumeration_id, listing, stability, reason, entries,
                                                      location_check, finished_at, end_order)
                     VALUES (?1, 'failed', 'unknown', ?2, 0, 'not_checked', ?3, ?4)",
                    params![enum_id, reason, now_ms(), end_order],
                )?;
                tx.commit()?;
                self.counts.listing_failures += 1;
                let msg = format!("cannot list {}: {reason}", display_dir(rel));
                self.gap(Some(loc), "listing_failed", &msg)?;
                return Ok(Listing {
                    dir_obs: None,
                    located: false,
                    entries: Vec::new(),
                    usable: false,
                    retryable: false,
                });
            }
        };
        let opened_at = now_ms();
        let (mut names, list_err) = fsx::list_dir(&fd);
        names.sort_by(|a, b| a.name.cmp(&b.name));
        let duplicates = names.windows(2).any(|w| w[0].name == w[1].name);
        let mut measured = Vec::with_capacity(names.len());
        for n in names {
            let m = fsx::cstr(&n.name)
                .and_then(|c| fsx::meta_at(&fd, &c))
                .map_err(|e| fsx::errno_name(&e));
            measured.push((n.name, m, now_ms()));
        }
        let after = fsx::meta_of(&fd).ok();
        let stable = after.is_some_and(|a| a.stable_against(&before));
        let lc = self.location_check(rel, &before);
        let (listing, reason) = match (&list_err, duplicates) {
            (Some(e), _) => ("incomplete", Some(format!("reading entries failed: {}", fsx::errno_name(e)))),
            (None, true) => ("incomplete", Some("duplicate names in one listing".to_string())),
            (None, false) if !stable => ("complete", Some("the directory changed while being listed".to_string())),
            _ => ("complete", None),
        };
        let stability = if after.is_none() {
            "unknown"
        } else if stable {
            "stable"
        } else {
            "unstable"
        };

        let tx = crate::catalog::atomic(self.conn)?;
        let dir_obs = self.insert_obs(loc, parent_obs, "opened", None, Ok(&before), false, opened_at)?;
        tx.execute(
            "INSERT INTO enumerations (scan_id, location_id, trigger_obs_id, dir_obs_id, attempt,
                                       started_at, start_order)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![self.scan_id, loc, trigger, dir_obs, attempt, started_at, start_order],
        )?;
        let enum_id = tx.last_insert_rowid();
        let mut entries = Vec::with_capacity(measured.len());
        for (name, m, at) in &measured {
            let child = self.child_location(loc, name)?;
            let boundary = matches!(m, Ok(m) if m.dev != before.dev || m.mount_root);
            let obs = self.insert_obs(
                child,
                Some(dir_obs),
                "entry",
                Some(enum_id),
                m.as_ref().map_err(|e| e.as_str()),
                boundary,
                *at,
            )?;
            let kind = m.as_ref().ok().map(|m| m.kind);
            match kind {
                Some(Kind::Symlink) => self.counts.symlinks += 1,
                Some(Kind::Other) => self.counts.other_types += 1,
                _ => {}
            }
            if boundary {
                self.counts.boundaries += 1;
            }
            entries.push(ListedEntry {
                loc: child,
                path: pathenc::join(rel, name),
                obs,
                kind,
                boundary,
            });
        }
        let end_order = self.next_order();
        tx.execute(
            "INSERT INTO enumeration_results (enumeration_id, listing, stability, reason, entries,
                                              after_mtime_ns, after_ctime_ns, location_check,
                                              location_reason, finished_at, end_order)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                enum_id,
                listing,
                stability,
                reason,
                entries.len() as i64,
                after.map(|a| a.mtime_ns),
                after.map(|a| a.ctime_ns),
                lc.status,
                lc.reason,
                lc.at,
                end_order
            ],
        )?;
        tx.commit()?;

        self.counts.dirs_listed += 1;
        self.counts.entries += entries.len() as u64;
        self.progress.dirs_listed += 1;
        self.progress.entries_seen += entries.len() as u64;
        for (name, m, _) in &measured {
            if let Err(e) = m {
                let msg = format!("cannot measure {}: {e}", pathenc::display(&pathenc::join(rel, name)));
                self.diag(Some(loc), "warning", "entry_metadata_failed", &msg)?;
            }
        }
        let usable = listing == "complete" && stable && lc.status == "confirmed";
        let retryable = !usable && list_err.is_none() && lc.status == "confirmed";
        if !usable {
            let detail = reason
                .clone()
                .or_else(|| lc.reason.clone())
                .unwrap_or_else(|| "listing unusable".into());
            let msg = format!("listing of {} is not usable coverage: {detail}", display_dir(rel));
            if retryable && attempt <= RETRY_LIMIT {
                self.diag(Some(loc), "info", "listing_retry", &format!("{msg}; retrying"))?;
            } else {
                self.counts.listing_failures += 1;
                let code = if lc.status != "confirmed" { "location_unconfirmed" } else { "listing_unstable" };
                self.gap(Some(loc), code, &msg)?;
            }
        }
        Ok(Listing {
            dir_obs: Some(dir_obs),
            located: lc.status == "confirmed",
            entries,
            usable,
            retryable,
        })
    }

    // ----- reads ---------------------------------------------------------

    fn open_for_read(&mut self, rel: &[u8]) -> std::result::Result<(File, Meta, i64), io::Error> {
        let f = fsx::open_file(&self.root, rel)?;
        let m = fsx::meta_of(&f)?;
        Ok((f, m, now_ms()))
    }

    #[allow(clippy::too_many_arguments)]
    fn record_open_failure(
        &mut self,
        loc: i64,
        rel: &[u8],
        trigger: Option<i64>,
        attempt: u32,
        method: &str,
        started_at: i64,
        start_order: i64,
        err: &io::Error,
    ) -> Result<()> {
        let reason = format!("open failed: {}", fsx::errno_name(err));
        let tx = crate::catalog::atomic(self.conn)?;
        tx.execute(
            "INSERT INTO reads (scan_id, location_id, trigger_obs_id, opened_obs_id, attempt, method,
                                started_at, start_order)
             VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6, ?7)",
            params![self.scan_id, loc, trigger, attempt, method, started_at, start_order],
        )?;
        let read_id = tx.last_insert_rowid();
        let end_order = self.next_order();
        tx.execute(
            "INSERT INTO read_results (read_id, content, reason, bytes_read, location_check,
                                       finished_at, end_order)
             VALUES (?1, 'unreadable', ?2, 0, 'not_checked', ?3, ?4)",
            params![read_id, reason, now_ms(), end_order],
        )?;
        tx.commit()?;
        self.counts.unreadable += 1;
        let msg = format!("cannot read {}: {reason}", pathenc::display(rel));
        self.gap(Some(loc), "unreadable", &msg)
    }

    /// Insert the fresh opened observation and the read attempt.
    #[allow(clippy::too_many_arguments)]
    fn begin_read(
        &mut self,
        loc: i64,
        parent_obs: Option<i64>,
        trigger: Option<i64>,
        attempt: u32,
        method: &str,
        compared: Option<i64>,
        compare_source: Option<&str>,
        meta: &Meta,
        opened_at: i64,
        started_at: i64,
        start_order: i64,
    ) -> Result<i64> {
        let tx = crate::catalog::atomic(self.conn)?;
        let opened = self.insert_obs(loc, parent_obs, "opened", None, Ok(meta), false, opened_at)?;
        let policy = (method != "size").then_some(self.policy_id);
        tx.execute(
            "INSERT INTO reads (scan_id, location_id, trigger_obs_id, opened_obs_id, attempt, method,
                                compared_blob_id, compare_source, policy_id, started_at, start_order)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                self.scan_id,
                loc,
                trigger,
                opened,
                attempt,
                method,
                compared,
                compare_source,
                policy,
                started_at,
                start_order
            ],
        )?;
        let read_id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(read_id)
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_result(
        &mut self,
        read_id: i64,
        content: Content,
        reason: Option<&str>,
        bytes: u64,
        blob: Option<i64>,
        after: Option<&Meta>,
        lc: &LocCheck,
    ) -> Result<()> {
        let end_order = self.next_order();
        self.conn
            .prepare_cached(
                "INSERT INTO read_results (read_id, content, reason, bytes_read, recognized_blob_id,
                                           after_size, after_mtime_ns, after_ctime_ns, location_check,
                                           location_reason, location_checked_at, finished_at, end_order)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            )?
            .execute(params![
                read_id,
                content.as_str(),
                reason,
                bytes as i64,
                blob,
                after.map(|a| a.size as i64),
                after.map(|a| a.mtime_ns),
                after.map(|a| a.ctime_ns),
                lc.status,
                lc.reason,
                (lc.status != "not_checked").then_some(lc.at),
                now_ms(),
                end_order
            ])?;
        Ok(())
    }

    /// Bookkeeping after a terminal read result. `final_attempt` is false
    /// when an unstable read will be retried.
    fn after_read(&mut self, loc: i64, rel: &[u8], content: Content, lc: &LocCheck, final_attempt: bool) -> Result<()> {
        let path = pathenc::display(rel);
        match content {
            Content::FullRecognition => self.counts.files_verified += 1,
            Content::SampleMatch => self.counts.sample_match += 1,
            Content::SampleMismatch => self.counts.sample_mismatch += 1,
            Content::SizeMismatch => self.counts.size_mismatch += 1,
            Content::NoBaseline => self.counts.no_baseline += 1,
            Content::Unreadable => self.counts.unreadable += 1,
            Content::Unstable => self.counts.unstable += 1,
            Content::NotRegular => self.counts.not_regular += 1,
            Content::IntegrityError => self.counts.integrity_errors += 1,
        }
        self.progress.files_read += 1;
        match content {
            Content::Unstable if !final_attempt => {
                self.counts.read_retries += 1;
                self.diag(Some(loc), "info", "read_retry", &format!("{path} changed while being read; retrying"))?;
                return Ok(());
            }
            Content::Unstable => {
                self.gap(Some(loc), "unstable", &format!("{path} kept changing while being read"))?;
            }
            Content::Unreadable => {
                self.gap(Some(loc), "unreadable", &format!("reading {path} failed"))?;
            }
            Content::NotRegular => {
                self.gap(Some(loc), "not_regular", &format!("{path} is no longer a regular file"))?;
            }
            Content::IntegrityError => {
                self.integrity_failure = true;
                self.gap(Some(loc), "integrity_error", &format!("{path}: recognized content conflicts with the catalog"))?;
            }
            _ => {}
        }
        if lc.status != "confirmed" && lc.status != "not_checked" {
            self.counts.location_unconfirmed += 1;
            let msg = format!(
                "location of {path} is {}: {}",
                lc.status,
                lc.reason.as_deref().unwrap_or("unknown reason")
            );
            self.gap(Some(loc), "location_unconfirmed", &msg)?;
        }
        self.report(false);
        Ok(())
    }

    fn verify_file(&mut self, loc: i64, rel: &[u8], parent_obs: Option<i64>, trigger: Option<i64>) -> Result<()> {
        for attempt in 1..=1 + RETRY_LIMIT {
            if self.check_cancel() {
                return Ok(());
            }
            self.batch()?;
            let started_at = now_ms();
            let start_order = self.next_order();
            self.set_current(rel);
            let (mut file, meta, opened_at) = match self.open_for_read(rel) {
                Ok(x) => x,
                Err(e) => return self.record_open_failure(loc, rel, trigger, attempt, "full", started_at, start_order, &e),
            };
            let read_id = self.begin_read(loc, parent_obs, trigger, attempt, "full", None, None, &meta, opened_at, started_at, start_order)?;
            let content = if meta.kind != Kind::File {
                let lc = self.location_check(rel, &meta);
                self.insert_result(read_id, Content::NotRegular, Some("not a regular file"), 0, None, None, &lc)?;
                self.after_read(loc, rel, Content::NotRegular, &lc, true)?;
                Content::NotRegular
            } else {
                self.full_read(read_id, loc, rel, &mut file, &meta, attempt)?
            };
            if content != Content::Unstable {
                break;
            }
        }
        Ok(())
    }

    /// Stream the whole file through the hasher, the sample token, and the
    /// streaming extractors; then check stability and location and commit.
    fn full_read(&mut self, read_id: i64, loc: i64, rel: &[u8], file: &mut File, meta: &Meta, attempt: u32) -> Result<Content> {
        // No write transaction stays open while a large file is hashed.
        if meta.size >= LARGE_READ {
            self.commit_batch()?;
        }
        let s = self.stream(file, meta);
        let lc = self.location_check(rel, meta);
        self.batch()?;
        let stable = s.error.is_none() && s.after.is_some_and(|a| a.stable_against(meta)) && s.bytes == meta.size;
        let tx = crate::catalog::atomic(self.conn)?;
        let (content, reason, blob) = if let Some(err) = &s.error {
            (Content::Unreadable, Some(err.clone()), None)
        } else if !stable {
            (Content::Unstable, Some("the file changed while being read".to_string()), None)
        } else {
            self.admit(read_id, loc, meta, &s)?
        };
        self.insert_result(read_id, content, reason.as_deref(), s.bytes, blob, s.after.as_ref(), &lc)?;
        tx.commit()?;
        self.after_read(loc, rel, content, &lc, content != Content::Unstable || attempt > RETRY_LIMIT)?;
        Ok(content)
    }

    fn stream(&mut self, file: &mut File, meta: &Meta) -> Stream {
        let mut hasher = blake3::Hasher::new();
        let mut token = match self.policy.plan(meta.size) {
            Plan::Full => None,
            Plan::Blocks(b) => Some(TokenHasher::new(&self.policy, meta.size, b)),
        };
        let mut extractors = extract::default_stream_extractors();
        let mut buf = std::mem::take(&mut self.buf);
        let mut offset = 0u64;
        let mut error = None;
        loop {
            match file.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let chunk = &buf[..n];
                    hasher.update(chunk);
                    if let Some(t) = token.as_mut() {
                        t.feed(offset, chunk);
                    }
                    for e in extractors.iter_mut() {
                        e.feed(chunk);
                    }
                    offset += n as u64;
                    self.counts.bytes_read += n as u64;
                    self.report(false);
                    if offset > meta.size {
                        // Grew while reading; stop: the stability check fails.
                        break;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    error = Some(format!("read failed after {offset} bytes: {}", fsx::errno_name(&e)));
                    break;
                }
            }
        }
        self.buf = buf;
        // Seeking extractors would run here, on the same handle, before the
        // final handle comparison. None ship in this prototype.
        let after = fsx::meta_of(&*file).ok();
        let outputs = if error.is_none() {
            extractors
                .iter_mut()
                .filter_map(|e| Some((e.name(), e.version(), e.config(), e.finish()?)))
                .collect()
        } else {
            Vec::new()
        };
        Stream {
            digest: Some(*hasher.finalize().as_bytes()),
            token: token.and_then(TokenHasher::finish),
            bytes: offset,
            error,
            after,
            outputs,
        }
    }

    /// Recognize or admit the Blob, its baseline, and extractor outputs for
    /// an accepted full read. Runs inside the result transaction.
    fn admit(&mut self, read_id: i64, loc: i64, meta: &Meta, s: &Stream) -> Result<(Content, Option<String>, Option<i64>)> {
        let digest = s.digest.expect("digest of a complete read");
        let existing: Option<(i64, i64)> = self
            .conn
            .prepare_cached("SELECT id, size FROM blobs WHERE digest = ?1")?
            .query_row([digest.as_slice()], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        let blob_id = match existing {
            Some((_, size)) if size != meta.size as i64 => {
                self.diag(Some(loc), "error", "integrity_error", "recognized content has a different recorded size")?;
                return Ok((Content::IntegrityError, Some("size_conflict".into()), None));
            }
            Some((id, _)) => {
                if let Some(token) = s.token {
                    let stored: Option<Vec<u8>> = self
                        .conn
                        .prepare_cached("SELECT token FROM baselines WHERE blob_id = ?1 AND policy_id = ?2")?
                        .query_row(params![id, self.policy_id], |r| r.get(0))
                        .optional()?;
                    if stored.is_some_and(|t| t != token) {
                        let public: String =
                            self.conn.query_row("SELECT public_id FROM blobs WHERE id = ?1", [id], |r| r.get(0))?;
                        let msg = format!(
                            "baseline conflict for {public} under policy {}",
                            self.policy.encoding()
                        );
                        self.diag(Some(loc), "error", "baseline_conflict", &msg)?;
                        return Ok((Content::IntegrityError, Some("baseline_conflict".into()), None));
                    }
                }
                self.counts.blobs_recognized += 1;
                id
            }
            None => {
                self.conn
                    .prepare_cached(
                        "INSERT INTO blobs (public_id, digest, size, admitted_from_read_id, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                    )?
                    .execute(params![new_public_id("blob"), digest.as_slice(), meta.size as i64, read_id, now_ms()])?;
                self.counts.blobs_new += 1;
                self.conn.last_insert_rowid()
            }
        };
        if let Some(token) = s.token {
            self.conn
                .prepare_cached(
                    "INSERT INTO baselines (blob_id, policy_id, token, created_from_read_id, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT DO NOTHING",
                )?
                .execute(params![blob_id, self.policy_id, token.as_slice(), read_id, now_ms()])?;
        }
        for (name, version, config, payload) in &s.outputs {
            let payload = payload.to_string();
            let stored: Option<(String, i64)> = self
                .conn
                .prepare_cached(
                    "SELECT payload, scan_id FROM derived_metadata
                     WHERE blob_id = ?1 AND extractor = ?2 AND version = ?3 AND config = ?4",
                )?
                .query_row(params![blob_id, name, version, config], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?;
            match stored {
                Some((p, _)) if p == payload => {}
                Some((_, other_scan)) => {
                    let msg = format!(
                        "extractor {name}/{version} produced a different result than in scan #{other_scan}; keeping the earlier result"
                    );
                    self.diag(Some(loc), "warning", "extractor_output_conflict", &msg)?;
                }
                None => {
                    self.conn
                        .prepare_cached(
                            "INSERT INTO derived_metadata (blob_id, extractor, version, config, payload,
                                                           scan_id, read_id, created_at)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                        )?
                        .execute(params![blob_id, name, version, config, payload, self.scan_id, read_id, now_ms()])?;
                }
            }
        }
        Ok((Content::FullRecognition, None, Some(blob_id)))
    }

    /// The Blob to compare the opened object against, from the projection as
    /// of the last publication. If the opened object is not the one recorded
    /// at this path (a replacement), the path's last verified content is the
    /// target: a comparison, not an identity claim. Never inferred from inode
    /// equality with other paths.
    fn comparison_target(&self, loc: i64, opened_ino: u64) -> Result<Comparison> {
        #[allow(clippy::type_complexity)]
        let current: Option<(String, Option<i64>, Option<i64>, Option<i64>, Option<i64>)> = self
            .conn
            .query_row(
                "SELECT content, content_blob_id, prev_blob_id, verified_blob_id, ino FROM occurrences
                 WHERE epoch_id = ?1 AND location_id = ?2 AND ended_seq IS NULL",
                params![self.epoch_id, loc],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        let (blob, source) = match current {
            Some((c, b, prev, verified, ino)) if ino != Some(opened_ino as i64) => {
                let last = if c == "verified" || c == "plausible" { b } else { verified.or(prev) };
                (last, "location_history")
            }
            Some((c, b, _, _, _)) if c == "verified" || c == "plausible" => (b, "occurrence"),
            Some((c, _, _, _, _)) if c == "changed" => (None, "contradicted"),
            Some((_, _, prev, _, _)) => (prev, "location_history"),
            None => (
                self.conn
                    .query_row(
                        "SELECT coalesce(verified_blob_id, prev_blob_id) FROM occurrences
                         WHERE location_id = ?1 AND coalesce(verified_blob_id, prev_blob_id) IS NOT NULL
                         ORDER BY id DESC LIMIT 1",
                        [loc],
                        |r| r.get(0),
                    )
                    .optional()?,
                "location_history",
            ),
        };
        let source = if blob.is_none() && source != "contradicted" { "none" } else { source };
        let blob = match blob {
            Some(id) => {
                let size: i64 = self.conn.query_row("SELECT size FROM blobs WHERE id = ?1", [id], |r| r.get(0))?;
                Some((id, size as u64))
            }
            None => None,
        };
        Ok(Comparison { blob, source })
    }

    fn check_file(&mut self, loc: i64, rel: &[u8], parent_obs: Option<i64>, trigger: Option<i64>) -> Result<()> {
        for attempt in 1..=1 + RETRY_LIMIT {
            if self.check_cancel() {
                return Ok(());
            }
            self.batch()?;
            let started_at = now_ms();
            let start_order = self.next_order();
            self.set_current(rel);
            let (mut file, meta, opened_at) = match self.open_for_read(rel) {
                Ok(x) => x,
                Err(e) => return self.record_open_failure(loc, rel, trigger, attempt, "sample", started_at, start_order, &e),
            };
            let cmp = self.comparison_target(loc, meta.ino)?;
            let compared = cmp.blob.map(|b| b.0);
            let source = (cmp.source != "none").then_some(cmp.source);
            let last = attempt > RETRY_LIMIT;
            if meta.kind != Kind::File {
                let read_id = self.begin_read(loc, parent_obs, trigger, attempt, "full", compared, source, &meta, opened_at, started_at, start_order)?;
                let lc = self.location_check(rel, &meta);
                self.insert_result(read_id, Content::NotRegular, Some("not a regular file"), 0, None, None, &lc)?;
                self.after_read(loc, rel, Content::NotRegular, &lc, true)?;
                return Ok(());
            }
            let plan = self.policy.plan(meta.size);
            let content = match (cmp.blob, plan) {
                (Some((_, size)), _) if size != meta.size => {
                    // Stable, location-confirmed size evidence; no content read.
                    let read_id = self.begin_read(loc, parent_obs, trigger, attempt, "size", compared, source, &meta, opened_at, started_at, start_order)?;
                    let after = fsx::meta_of(&file).ok();
                    let lc = self.location_check(rel, &meta);
                    let content = if after.is_some_and(|a| a.stable_against(&meta)) {
                        Content::SizeMismatch
                    } else {
                        Content::Unstable
                    };
                    self.insert_result(read_id, content, None, 0, None, after.as_ref(), &lc)?;
                    self.after_read(loc, rel, content, &lc, content != Content::Unstable || last)?;
                    content
                }
                (_, Plan::Full) => {
                    let read_id = self.begin_read(loc, parent_obs, trigger, attempt, "full", compared, source, &meta, opened_at, started_at, start_order)?;
                    self.full_read(read_id, loc, rel, &mut file, &meta, attempt)?
                }
                (None, Plan::Blocks(_)) => {
                    let read_id = self.begin_read(loc, parent_obs, trigger, attempt, "sample", None, source, &meta, opened_at, started_at, start_order)?;
                    let lc = self.location_check(rel, &meta);
                    let reason = if cmp.source == "contradicted" {
                        "the last comparison ruled out the expected content; Verify to identify it"
                    } else {
                        "no previously verified content is known at this location"
                    };
                    self.insert_result(read_id, Content::NoBaseline, Some(reason), 0, None, None, &lc)?;
                    self.after_read(loc, rel, Content::NoBaseline, &lc, true)?;
                    Content::NoBaseline
                }
                (Some((blob_id, _)), Plan::Blocks(blocks)) => {
                    let baseline: Option<Vec<u8>> = self
                        .conn
                        .query_row(
                            "SELECT token FROM baselines WHERE blob_id = ?1 AND policy_id = ?2",
                            params![blob_id, self.policy_id],
                            |r| r.get(0),
                        )
                        .optional()?;
                    let read_id = self.begin_read(loc, parent_obs, trigger, attempt, "sample", compared, source, &meta, opened_at, started_at, start_order)?;
                    match baseline {
                        None => {
                            let lc = self.location_check(rel, &meta);
                            self.insert_result(read_id, Content::NoBaseline, Some("no baseline under the current sampling policy"), 0, None, None, &lc)?;
                            self.after_read(loc, rel, Content::NoBaseline, &lc, true)?;
                            Content::NoBaseline
                        }
                        Some(expected) => {
                            let sampled = sample::sample_file(&file, &self.policy, meta.size, &blocks);
                            let after = fsx::meta_of(&file).ok();
                            let lc = self.location_check(rel, &meta);
                            let (content, reason, bytes) = match sampled {
                                Err(e) => (Content::Unreadable, Some(format!("sampled read failed: {}", fsx::errno_name(&e))), 0),
                                Ok((_, n)) if !after.is_some_and(|a| a.stable_against(&meta)) => {
                                    (Content::Unstable, Some("the file changed while being sampled".into()), n)
                                }
                                Ok((token, n)) if token.as_slice() == expected.as_slice() => (Content::SampleMatch, None, n),
                                Ok((_, n)) => (Content::SampleMismatch, None, n),
                            };
                            self.counts.bytes_read += bytes;
                            self.insert_result(read_id, content, reason.as_deref(), bytes, None, after.as_ref(), &lc)?;
                            self.after_read(loc, rel, content, &lc, content != Content::Unstable || last)?;
                            content
                        }
                    }
                }
            };
            if content != Content::Unstable {
                break;
            }
        }
        Ok(())
    }
}

fn display_dir(rel: &[u8]) -> String {
    if rel.is_empty() {
        "the root directory".into()
    } else {
        pathenc::display(rel)
    }
}
