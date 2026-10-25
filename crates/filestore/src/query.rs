//! Read-side views over the projection and evidence, for frontends.
//!
//! Status codes are computed here, once, from the projection so that every
//! consumer presents the same interpretation:
//!
//! files    verified | modified | plausible | changed | unidentified
//! dirs     listed | stale | unlisted | boundary
//! others   symlink | other | unknown
//! history  gone (occurrence ended) | previous_epoch

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;
use serde_json::{Value, json};

use crate::ids::now_ms;
use crate::semantic::{self, AssignmentView};
use crate::{Catalog, Error, Result, pathenc};

const DAY_MS: i64 = 86_400_000;

#[derive(Serialize, Clone, Debug)]
pub struct CheckView {
    pub result: String,
    pub at: i64,
    pub blob: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct IssueView {
    pub code: String,
    pub detail: Option<String>,
    pub at: Option<i64>,
}

#[derive(Serialize, Clone, Debug)]
pub struct ListingView {
    pub listed_at: Option<i64>,
    pub scan_seq: Option<i64>,
    pub entries: Option<i64>,
    pub stale: bool,
    pub issue: Option<IssueView>,
}

#[derive(Serialize, Clone, Debug)]
pub struct EntryView {
    pub path: String,
    pub name: String,
    pub kind: Option<String>,
    pub status: String,
    pub present: bool,
    pub epoch: i64,
    pub ino: Option<i64>,
    pub size: Option<i64>,
    pub mtime_ms: Option<i64>,
    pub first_seen_at: i64,
    pub last_seen_at: i64,
    pub last_seen_seq: i64,
    pub ended_at: Option<i64>,
    pub end_reason: Option<String>,
    pub boundary: bool,
    pub meta_issue: Option<String>,
    /// Verified, plausible, or ruled-out Blob, depending on `status`.
    pub blob: Option<String>,
    pub blob_size: Option<i64>,
    pub content_at: Option<i64>,
    /// Which metadata differs from the read behind the content evidence.
    pub modified: Option<String>,
    pub verified_blob: Option<String>,
    pub verified_at: Option<i64>,
    pub overdue: bool,
    pub check: Option<CheckView>,
    pub prev_blob: Option<String>,
    pub issue: Option<IssueView>,
    pub listing: Option<ListingView>,
    pub format: Option<String>,
    /// Tags of `blob`. They describe this file only when `status` is verified.
    pub tags: Vec<AssignmentView>,
    pub lineages: i64,
    pub subtree: Option<Counts>,
}

#[derive(Serialize, Default, Clone, Debug)]
pub struct Counts {
    pub files: u64,
    pub bytes: u64,
    pub verified: u64,
    pub modified: u64,
    pub plausible: u64,
    pub changed: u64,
    pub unidentified: u64,
    pub issues: u64,
    pub overdue: u64,
    pub dirs: u64,
    pub dirs_unlisted: u64,
    pub dirs_stale: u64,
    pub dirs_issue: u64,
    pub boundaries: u64,
    pub other: u64,
}

impl Counts {
    fn add(&mut self, o: &Counts) {
        self.files += o.files;
        self.bytes += o.bytes;
        self.verified += o.verified;
        self.modified += o.modified;
        self.plausible += o.plausible;
        self.changed += o.changed;
        self.unidentified += o.unidentified;
        self.issues += o.issues;
        self.overdue += o.overdue;
        self.dirs += o.dirs;
        self.dirs_unlisted += o.dirs_unlisted;
        self.dirs_stale += o.dirs_stale;
        self.dirs_issue += o.dirs_issue;
        self.boundaries += o.boundaries;
        self.other += o.other;
    }
}

pub fn active_epoch(conn: &Connection) -> Result<Option<i64>> {
    Ok(conn.query_row("SELECT max(id) FROM epochs", [], |r| r.get(0))?)
}

fn horizon_ms(cat: &Catalog) -> i64 {
    cat.verify_horizon_days() * DAY_MS
}

pub fn file_status(content: &str, modified: bool) -> &'static str {
    match (content, modified) {
        ("verified" | "plausible", true) => "modified",
        ("verified", false) => "verified",
        ("plausible", false) => "plausible",
        ("changed", _) => "changed",
        _ => "unidentified",
    }
}

fn modified_field(
    size: Option<i64>,
    mtime: Option<i64>,
    ctime: Option<i64>,
    ev: (Option<i64>, Option<i64>, Option<i64>),
) -> Option<&'static str> {
    if size != ev.0 {
        Some("size")
    } else if mtime != ev.1 {
        Some("mtime")
    } else if ctime != ev.2 {
        Some("ctime")
    } else {
        None
    }
}

const OCC_COLUMNS: &str = "o.id, o.epoch_id, l.path, o.kind, o.ino, o.first_at, o.last_at, o.last_seq,
    o.size, o.mtime_ns, o.ctime_ns, o.boundary, o.meta_issue, o.ended_at, o.end_reason, o.content,
    cb.public_id, cb.size, o.content_at, o.ev_size, o.ev_mtime_ns, o.ev_ctime_ns, vb.public_id,
    o.verified_at, o.check_result, kb.public_id, o.check_at, pb.public_id, o.issue, o.issue_detail,
    o.issue_at, o.listing_enum_id, o.listing_at, o.listing_seq, o.listing_entries, o.listing_stale,
    o.listing_issue, o.listing_issue_at, o.content_blob_id";

const OCC_FROM: &str = "FROM occurrences o JOIN locations l ON l.id = o.location_id
    LEFT JOIN blobs cb ON cb.id = o.content_blob_id
    LEFT JOIN blobs vb ON vb.id = o.verified_blob_id
    LEFT JOIN blobs kb ON kb.id = o.check_blob_id
    LEFT JOIN blobs pb ON pb.id = o.prev_blob_id";

struct Built {
    occ_id: i64,
    content_blob_id: Option<i64>,
    view: EntryView,
}

fn build(r: &Row, active: Option<i64>, horizon: i64, now: i64) -> rusqlite::Result<Built> {
    let path: Vec<u8> = r.get(2)?;
    let kind: Option<String> = r.get(3)?;
    let epoch: i64 = r.get(1)?;
    let ended_at: Option<i64> = r.get(13)?;
    let present = ended_at.is_none() && Some(epoch) == active;
    let size: Option<i64> = r.get(8)?;
    let mtime: Option<i64> = r.get(9)?;
    let ctime: Option<i64> = r.get(10)?;
    let boundary: bool = r.get(11)?;
    let content: String = r.get(15)?;
    let ev = (r.get(19)?, r.get(20)?, r.get(21)?);
    let is_file = kind.as_deref() == Some("file");
    let modified = if is_file && (content == "verified" || content == "plausible") {
        modified_field(size, mtime, ctime, ev)
    } else {
        None
    };
    let listing_enum: Option<i64> = r.get(31)?;
    let listing_stale: bool = r.get(35)?;
    let listing_issue: Option<String> = r.get(36)?;
    let status = if !present {
        if ended_at.is_some() { "gone" } else { "previous_epoch" }
    } else {
        match kind.as_deref() {
            Some("file") => file_status(&content, modified.is_some()),
            Some("dir") if boundary => "boundary",
            Some("dir") if listing_enum.is_none() => "unlisted",
            Some("dir") if listing_stale => "stale",
            Some("dir") => "listed",
            Some("symlink") => "symlink",
            Some(_) => "other",
            None => "unknown",
        }
    };
    let verified_at: Option<i64> = r.get(23)?;
    let check = match r.get::<_, Option<String>>(24)? {
        Some(result) => Some(CheckView {
            result,
            at: r.get::<_, Option<i64>>(26)?.unwrap_or(0),
            blob: r.get(25)?,
        }),
        None => None,
    };
    let issue = r.get::<_, Option<String>>(28)?.map(|code| IssueView {
        code,
        detail: r.get(29).ok().flatten(),
        at: r.get(30).ok().flatten(),
    });
    let listing = (kind.as_deref() == Some("dir")).then(|| ListingView {
        listed_at: r.get(32).ok().flatten(),
        scan_seq: r.get(33).ok().flatten(),
        entries: r.get(34).ok().flatten(),
        stale: listing_stale,
        issue: listing_issue.map(|code| IssueView {
            code,
            detail: None,
            at: r.get(37).ok().flatten(),
        }),
    });
    let overdue = present && content == "verified" && verified_at.is_some_and(|v| now - v > horizon);
    let content_blob: Option<String> = r.get(16)?;
    Ok(Built {
        occ_id: r.get(0)?,
        content_blob_id: r.get(38)?,
        view: EntryView {
            name: pathenc::encode(pathenc::file_name(&path)),
            path: pathenc::encode(&path),
            kind,
            status: status.to_string(),
            present,
            epoch,
            ino: r.get(4)?,
            size,
            mtime_ms: mtime.map(|m| m.div_euclid(1_000_000)),
            first_seen_at: r.get(5)?,
            last_seen_at: r.get(6)?,
            last_seen_seq: r.get(7)?,
            ended_at,
            end_reason: r.get(14)?,
            boundary,
            meta_issue: r.get(12)?,
            blob: if content == "unidentified" { None } else { content_blob },
            blob_size: r.get(17)?,
            content_at: r.get(18)?,
            modified: modified.map(String::from),
            verified_blob: r.get(22)?,
            verified_at,
            overdue,
            check,
            prev_blob: r.get(27)?,
            issue,
            listing,
            format: None,
            tags: Vec::new(),
            lineages: 0,
            subtree: None,
        },
    })
}

/// Run an occurrence query and decorate rows with formats, tags, and
/// Lineage counts of their Blobs.
fn entries(cat: &Catalog, where_sql: &str, params: &[&dyn rusqlite::ToSql], limit: i64) -> Result<Vec<Built>> {
    let conn = &cat.conn;
    let active = active_epoch(conn)?;
    let horizon = horizon_ms(cat);
    let now = now_ms();
    let sql = format!("SELECT {OCC_COLUMNS} {OCC_FROM} WHERE {where_sql} LIMIT {limit}");
    let mut stmt = conn.prepare(&sql)?;
    let mut out: Vec<Built> = stmt
        .query_map(params, |r| build(r, active, horizon, now))?
        .collect::<rusqlite::Result<_>>()?;
    let blob_ids: Vec<i64> = {
        let mut v: Vec<i64> = out.iter().filter_map(|b| b.content_blob_id).collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    if !blob_ids.is_empty() {
        let ids_json = serde_json::to_string(&blob_ids)?;
        let mut formats: HashMap<i64, String> = HashMap::new();
        let mut stmt = conn.prepare(
            "SELECT blob_id, payload FROM derived_metadata
             WHERE extractor = 'sniff' AND blob_id IN (SELECT value FROM json_each(?1)) ORDER BY id",
        )?;
        for row in stmt.query_map([&ids_json], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
            let (b, payload) = row?;
            formats.insert(b, format_label(&payload));
        }
        let mut lineages: HashMap<i64, i64> = HashMap::new();
        let mut stmt = conn.prepare(
            "SELECT r.blob_id, count(DISTINCT r.lineage_id) FROM revisions r JOIN lineages l ON l.id = r.lineage_id
             WHERE r.tombstoned_at IS NULL AND l.tombstoned_at IS NULL
               AND r.blob_id IN (SELECT value FROM json_each(?1)) GROUP BY r.blob_id",
        )?;
        for row in stmt.query_map([&ids_json], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
            let (b, n) = row?;
            lineages.insert(b, n);
        }
        let mut tags = semantic::assignments_for(conn, "blob", &blob_ids)?;
        for b in out.iter_mut() {
            if let Some(id) = b.content_blob_id {
                b.view.format = formats.get(&id).cloned();
                b.view.lineages = lineages.get(&id).copied().unwrap_or(0);
                if let Some(t) = tags.get(&id) {
                    b.view.tags = t.clone();
                }
            }
        }
        tags.clear();
    }
    Ok(out)
}

pub fn format_label(payload: &str) -> String {
    let v: Value = serde_json::from_str(payload).unwrap_or(Value::Null);
    let mut s = v["format"].as_str().unwrap_or("?").to_string();
    if let Some(ver) = v["version"].as_str().filter(|s| !s.is_empty()) {
        s.push(' ');
        s.push_str(ver);
    }
    if let (Some(w), Some(h)) = (v["width"].as_u64(), v["height"].as_u64()) {
        s.push_str(&format!(" {w}×{h}"));
    }
    s
}

fn location_id(conn: &Connection, path: &[u8]) -> Result<Option<i64>> {
    Ok(conn
        .query_row("SELECT id FROM locations WHERE path = ?1", [path], |r| r.get(0))
        .optional()?)
}

// ----- subtree aggregates ---------------------------------------------------

pub struct Aggregates {
    key: (i64, i64, i64, i64),
    subtree: HashMap<i64, Counts>,
}

/// Subtree counts for every current directory occurrence, cached per
/// projection state.
pub fn aggregates(cat: &Catalog) -> Result<std::sync::Arc<Aggregates>> {
    let conn = &cat.conn;
    let epoch = active_epoch(conn)?.unwrap_or(0);
    let seq: i64 = crate::catalog::get_meta(conn, "projection_seq")?
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let horizon = horizon_ms(cat);
    // Overdue counts depend on the time; refresh them at least hourly.
    let key = (epoch, seq, horizon, now_ms() / 3_600_000);
    if let Some(a) = cat.aggregates.borrow().as_ref().filter(|a| a.key == key) {
        return Ok(a.clone());
    }
    let now = now_ms();
    let mut parent: HashMap<i64, i64> = HashMap::new();
    let mut own: Vec<(i64, Counts)> = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT id, parent_occ_id, kind, content, size, mtime_ns, ctime_ns, ev_size, ev_mtime_ns, ev_ctime_ns,
                issue, listing_enum_id, listing_stale, listing_issue, boundary, verified_at
         FROM occurrences WHERE epoch_id = ?1 AND ended_seq IS NULL",
    )?;
    let mut rows = stmt.query([epoch])?;
    while let Some(r) = rows.next()? {
        let id: i64 = r.get(0)?;
        if let Some(p) = r.get::<_, Option<i64>>(1)? {
            parent.insert(id, p);
        }
        let kind: Option<String> = r.get(2)?;
        let mut c = Counts::default();
        match kind.as_deref() {
            Some("file") => {
                let content: String = r.get(3)?;
                let size: Option<i64> = r.get(4)?;
                let modified = (content == "verified" || content == "plausible")
                    && modified_field(size, r.get(5)?, r.get(6)?, (r.get(7)?, r.get(8)?, r.get(9)?)).is_some();
                c.files = 1;
                c.bytes = size.unwrap_or(0).max(0) as u64;
                match file_status(&content, modified) {
                    "verified" => c.verified = 1,
                    "modified" => c.modified = 1,
                    "plausible" => c.plausible = 1,
                    "changed" => c.changed = 1,
                    _ => c.unidentified = 1,
                }
                if r.get::<_, Option<String>>(10)?.is_some() {
                    c.issues = 1;
                }
                if content == "verified" && r.get::<_, Option<i64>>(15)?.is_some_and(|v| now - v > horizon) {
                    c.overdue = 1;
                }
            }
            Some("dir") => {
                c.dirs = 1;
                if r.get::<_, bool>(14)? {
                    c.boundaries = 1;
                } else if r.get::<_, Option<i64>>(11)?.is_none() {
                    c.dirs_unlisted = 1;
                } else if r.get::<_, bool>(12)? {
                    c.dirs_stale = 1;
                }
                if r.get::<_, Option<String>>(13)?.is_some() {
                    c.dirs_issue = 1;
                }
            }
            _ => c.other = 1,
        }
        own.push((id, c));
    }
    let mut subtree: HashMap<i64, Counts> = HashMap::new();
    for (id, c) in &own {
        let mut cur = parent.get(id).copied();
        while let Some(p) = cur {
            subtree.entry(p).or_default().add(c);
            cur = parent.get(&p).copied();
        }
    }
    let agg = std::sync::Arc::new(Aggregates { key, subtree });
    *cat.aggregates.borrow_mut() = Some(agg.clone());
    Ok(agg)
}

impl Aggregates {
    pub fn of(&self, occ: i64) -> Counts {
        self.subtree.get(&occ).cloned().unwrap_or_default()
    }
}

// ----- directory views ------------------------------------------------------

#[derive(Serialize)]
pub struct Crumb {
    pub name: String,
    pub path: String,
}

#[derive(Serialize)]
pub struct DirView {
    pub path: String,
    pub crumbs: Vec<Crumb>,
    pub dir: Option<EntryView>,
    /// The latest occurrence at this location when none is current.
    pub previous: Option<EntryView>,
    pub entries: Vec<EntryView>,
    pub history: Vec<EntryView>,
    pub truncated: bool,
}

fn crumbs(path: &[u8]) -> Vec<Crumb> {
    let mut out = vec![Crumb {
        name: String::new(),
        path: String::new(),
    }];
    let mut acc: Vec<u8> = Vec::new();
    for c in pathenc::components(path) {
        acc = pathenc::join(&acc, c);
        out.push(Crumb {
            name: pathenc::encode(c),
            path: pathenc::encode(&acc),
        });
    }
    out
}

const ENTRY_LIMIT: i64 = 20_000;

pub fn dir_view(cat: &Catalog, path: &str, with_history: bool) -> Result<DirView> {
    let bytes = pathenc::decode_relative(path)?;
    let conn = &cat.conn;
    let epoch = active_epoch(conn)?.unwrap_or(0);
    let loc = location_id(conn, &bytes)?;
    let mut view = DirView {
        path: pathenc::encode(&bytes),
        crumbs: crumbs(&bytes),
        dir: None,
        previous: None,
        entries: Vec::new(),
        history: Vec::new(),
        truncated: false,
    };
    let Some(loc) = loc else {
        return Ok(view);
    };
    let agg = aggregates(cat)?;
    let current = entries(
        cat,
        "o.location_id = ?1 AND o.epoch_id = ?2 AND o.ended_seq IS NULL",
        &[&loc, &epoch],
        1,
    )?
    .pop();
    match current {
        Some(mut d) => {
            d.view.subtree = Some(agg.of(d.occ_id));
            let children = entries(
                cat,
                "o.parent_occ_id = ?1 AND o.ended_seq IS NULL ORDER BY l.name",
                &[&d.occ_id],
                ENTRY_LIMIT + 1,
            )?;
            view.truncated = children.len() as i64 > ENTRY_LIMIT;
            view.entries = children
                .into_iter()
                .take(ENTRY_LIMIT as usize)
                .map(|mut c| {
                    if c.view.kind.as_deref() == Some("dir") {
                        c.view.subtree = Some(agg.of(c.occ_id));
                    }
                    c.view
                })
                .collect();
            view.dir = Some(d.view);
        }
        None => {
            view.previous = entries(cat, "o.location_id = ?1 ORDER BY o.id DESC", &[&loc], 1)?
                .pop()
                .map(|b| b.view);
        }
    }
    if with_history {
        view.history = entries(
            cat,
            "l.parent_id = ?1
             AND o.id = (SELECT max(o2.id) FROM occurrences o2 WHERE o2.location_id = l.id)
             AND NOT EXISTS (SELECT 1 FROM occurrences c WHERE c.location_id = l.id
                             AND c.epoch_id = ?2 AND c.ended_seq IS NULL)
             ORDER BY l.name",
            &[&loc, &epoch],
            ENTRY_LIMIT,
        )?
        .into_iter()
        .map(|b| b.view)
        .collect();
    }
    Ok(view)
}

#[derive(Serialize)]
pub struct TreeNode {
    pub name: String,
    pub path: String,
    pub status: String,
    pub has_subdirs: bool,
    pub counts: Counts,
    pub listing: Option<ListingView>,
}

/// Current subdirectories of `path`, with subtree counts.
pub fn tree(cat: &Catalog, path: &str) -> Result<Vec<TreeNode>> {
    let bytes = pathenc::decode_relative(path)?;
    let conn = &cat.conn;
    let epoch = active_epoch(conn)?.unwrap_or(0);
    let Some(loc) = location_id(conn, &bytes)? else {
        return Ok(Vec::new());
    };
    let agg = aggregates(cat)?;
    let parent: Option<i64> = conn
        .query_row(
            "SELECT id FROM occurrences WHERE location_id = ?1 AND epoch_id = ?2 AND ended_seq IS NULL",
            params![loc, epoch],
            |r| r.get(0),
        )
        .optional()?;
    let Some(parent) = parent else {
        return Ok(Vec::new());
    };
    let dirs = entries(
        cat,
        "o.parent_occ_id = ?1 AND o.ended_seq IS NULL AND o.kind = 'dir' ORDER BY l.name",
        &[&parent],
        ENTRY_LIMIT,
    )?;
    Ok(dirs
        .into_iter()
        .map(|b| {
            let counts = agg.of(b.occ_id);
            TreeNode {
                name: b.view.name,
                path: b.view.path,
                status: b.view.status,
                has_subdirs: counts.dirs > 0,
                counts,
                listing: b.view.listing,
            }
        })
        .collect())
}

pub fn search(cat: &Catalog, q: &str, limit: i64) -> Result<Vec<EntryView>> {
    let q = q.trim();
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let epoch = active_epoch(&cat.conn)?.unwrap_or(0);
    let pattern = format!(
        "%{}%",
        q.to_lowercase().replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
    );
    Ok(entries(
        cat,
        "o.epoch_id = ?1 AND o.ended_seq IS NULL AND l.path != X''
         AND lower(CAST(l.path AS TEXT)) LIKE ?2 ESCAPE '\\' ORDER BY l.path",
        &[&epoch, &pattern],
        limit,
    )?
    .into_iter()
    .map(|b| b.view)
    .collect())
}

// ----- location detail ------------------------------------------------------

#[derive(Serialize)]
pub struct EventView {
    pub at: i64,
    pub order: i64,
    pub scan: i64,
    pub operation: String,
    pub published: bool,
    pub outcome: Option<String>,
    pub kind: String,
    pub summary: String,
    pub ok: Option<bool>,
}

#[derive(Serialize)]
pub struct Related {
    pub path: String,
    pub status: String,
    pub present: bool,
    pub ended_at: Option<i64>,
}

#[derive(Serialize)]
pub struct LocationDetail {
    pub path: String,
    pub current: Option<EntryView>,
    pub occurrences: Vec<EntryView>,
    pub events: Vec<EventView>,
    pub same_inode: Vec<Related>,
    pub revisions: Vec<semantic::RevisionRef>,
}

fn human_size(n: i64) -> String {
    let n = n as f64;
    if n < 1024.0 {
        format!("{n} B")
    } else if n < 1024.0 * 1024.0 {
        format!("{:.1} KiB", n / 1024.0)
    } else if n < 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} MiB", n / 1024.0 / 1024.0)
    } else {
        format!("{:.2} GiB", n / 1024.0 / 1024.0 / 1024.0)
    }
}

pub fn location_detail(cat: &Catalog, path: &str) -> Result<LocationDetail> {
    let bytes = pathenc::decode_relative(path)?;
    let conn = &cat.conn;
    let loc = location_id(conn, &bytes)?.ok_or_else(|| Error::NotFound(format!("location {path:?}")))?;
    let epoch = active_epoch(conn)?.unwrap_or(0);
    let occs = entries(cat, "o.location_id = ?1 ORDER BY o.id DESC", &[&loc], 200)?;
    let current = occs
        .iter()
        .find(|b| b.view.present)
        .map(|b| b.view.clone());

    let mut events: Vec<EventView> = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT o.observed_at, o.event_order, s.id, s.operation, s.publication_seq IS NOT NULL, s.outcome,
                    o.role, o.kind, o.ino, o.size, o.error, o.boundary
             FROM observations o JOIN scans s ON s.id = o.scan_id
             WHERE o.location_id = ?1 AND o.role IN ('root', 'anchor', 'entry')
             ORDER BY o.id DESC LIMIT 300",
        )?;
        let rows = stmt.query_map([loc], |r| {
            let role: String = r.get(6)?;
            let kind: Option<String> = r.get(7)?;
            let ino: Option<i64> = r.get(8)?;
            let size: Option<i64> = r.get(9)?;
            let error: Option<String> = r.get(10)?;
            let how = match role.as_str() {
                "entry" => "Seen in parent listing",
                "anchor" => "Resolved as a path component",
                _ => "Opened as the root",
            };
            let summary = match (&error, &kind) {
                (Some(e), _) => format!("{how}; metadata unavailable ({e})"),
                (None, Some(k)) => {
                    let mut s = format!("{how}: {k}, inode {}", ino.unwrap_or(0));
                    if k == "file" {
                        s.push_str(&format!(", {}", human_size(size.unwrap_or(0))));
                    }
                    if r.get::<_, bool>(11)? {
                        s.push_str(", on another filesystem (not entered)");
                    }
                    s
                }
                (None, None) => how.to_string(),
            };
            Ok(EventView {
                at: r.get(0)?,
                order: r.get(1)?,
                scan: r.get(2)?,
                operation: r.get(3)?,
                published: r.get(4)?,
                outcome: r.get(5)?,
                kind: "observation".into(),
                summary,
                ok: error.is_none().then_some(true).or(Some(false)),
            })
        })?;
        for e in rows {
            events.push(e?);
        }
    }
    {
        let mut stmt = conn.prepare(
            "SELECT x.finished_at, x.end_order, s.id, s.operation, s.publication_seq IS NOT NULL, s.outcome,
                    e.attempt, x.listing, x.stability, x.reason, x.entries, x.location_check, x.location_reason,
                    d.ino
             FROM enumerations e JOIN enumeration_results x ON x.enumeration_id = e.id
             JOIN scans s ON s.id = e.scan_id LEFT JOIN observations d ON d.id = e.dir_obs_id
             WHERE e.location_id = ?1 ORDER BY e.id DESC LIMIT 200",
        )?;
        let rows = stmt.query_map([loc], |r| {
            let listing: String = r.get(7)?;
            let stability: String = r.get(8)?;
            let reason: Option<String> = r.get(9)?;
            let lc: String = r.get(11)?;
            let attempt: i64 = r.get(6)?;
            let usable = listing == "complete" && stability == "stable" && lc == "confirmed";
            let mut summary = if listing == "failed" {
                format!("Listing failed: {}", reason.clone().unwrap_or_default())
            } else {
                format!(
                    "Listed {} entries (inode {}): {listing}, {stability}, location {lc}",
                    r.get::<_, i64>(10)?,
                    r.get::<_, Option<i64>>(13)?.unwrap_or(0)
                )
            };
            if let (Some(reason), true) = (&reason, listing != "failed") {
                summary.push_str(&format!(" — {reason}"));
            }
            if let Some(lr) = r.get::<_, Option<String>>(12)? {
                summary.push_str(&format!(" — {lr}"));
            }
            if attempt > 1 {
                summary.push_str(&format!(" (attempt {attempt})"));
            }
            Ok(EventView {
                at: r.get(0)?,
                order: r.get(1)?,
                scan: r.get(2)?,
                operation: r.get(3)?,
                published: r.get(4)?,
                outcome: r.get(5)?,
                kind: "listing".into(),
                summary,
                ok: Some(usable),
            })
        })?;
        for e in rows {
            events.push(e?);
        }
    }
    {
        let mut stmt = conn.prepare(
            "SELECT x.finished_at, x.end_order, s.id, s.operation, s.publication_seq IS NOT NULL, s.outcome,
                    r.attempt, r.method, x.content, x.reason, x.bytes_read, rb.public_id, cb.public_id,
                    x.location_check, x.location_reason, o.ino, o.size, r.compare_source
             FROM reads r JOIN read_results x ON x.read_id = r.id JOIN scans s ON s.id = r.scan_id
             LEFT JOIN blobs rb ON rb.id = x.recognized_blob_id
             LEFT JOIN blobs cb ON cb.id = r.compared_blob_id
             LEFT JOIN observations o ON o.id = r.opened_obs_id
             WHERE r.location_id = ?1 ORDER BY r.id DESC LIMIT 200",
        )?;
        let rows = stmt.query_map([loc], |r| {
            let method: String = r.get(7)?;
            let content: String = r.get(8)?;
            let reason: Option<String> = r.get(9)?;
            let bytes: i64 = r.get(10)?;
            let recognized: Option<String> = r.get(11)?;
            let compared: Option<String> = r.get(12)?;
            let lc: String = r.get(13)?;
            let attempt: i64 = r.get(6)?;
            let opened = match (r.get::<_, Option<i64>>(15)?, r.get::<_, Option<i64>>(16)?) {
                (Some(ino), Some(size)) => format!(" of inode {ino} ({})", human_size(size)),
                _ => String::new(),
            };
            let vs = compared.as_deref().map(|c| format!(" against {c}")).unwrap_or_default();
            let mut summary = match content.as_str() {
                "full_recognition" => format!(
                    "Full read{opened}: {} → {}",
                    human_size(bytes),
                    recognized.clone().unwrap_or_default()
                ),
                "sample_match" => format!("Sampled{opened}{vs}: match ({} read)", human_size(bytes)),
                "sample_mismatch" => format!("Sampled{opened}{vs}: MISMATCH ({} read)", human_size(bytes)),
                "size_mismatch" => format!("Size check{opened}{vs}: size differs, no content read"),
                "no_baseline" => format!("Check{opened}: content unresolved — {}", reason.clone().unwrap_or_default()),
                "unreadable" => format!("{method} read failed: {}", reason.clone().unwrap_or_default()),
                "unstable" => format!("{method} read{opened} unstable: {}", reason.clone().unwrap_or_default()),
                "not_regular" => "Not a regular file; nothing read".to_string(),
                _ => format!("{content}: {}", reason.clone().unwrap_or_default()),
            };
            if lc != "not_checked" {
                summary.push_str(&format!("; location {lc}"));
                if let Some(lr) = r.get::<_, Option<String>>(14)? {
                    summary.push_str(&format!(" ({lr})"));
                }
            }
            if attempt > 1 {
                summary.push_str(&format!(" (attempt {attempt})"));
            }
            let ok = match content.as_str() {
                "full_recognition" | "sample_match" => lc == "confirmed",
                "no_baseline" => return Ok(EventView {
                    at: r.get(0)?,
                    order: r.get(1)?,
                    scan: r.get(2)?,
                    operation: r.get(3)?,
                    published: r.get(4)?,
                    outcome: r.get(5)?,
                    kind: "read".into(),
                    summary,
                    ok: None,
                }),
                _ => false,
            };
            Ok(EventView {
                at: r.get(0)?,
                order: r.get(1)?,
                scan: r.get(2)?,
                operation: r.get(3)?,
                published: r.get(4)?,
                outcome: r.get(5)?,
                kind: "read".into(),
                summary,
                ok: Some(ok),
            })
        })?;
        for e in rows {
            events.push(e?);
        }
    }
    events.sort_by_key(|e| std::cmp::Reverse((e.scan, e.order)));
    events.truncate(300);

    let mut same_inode = Vec::new();
    if let Some(cur) = occs.iter().find(|b| b.view.epoch == epoch && b.view.ino.is_some()) {
        let mut stmt = conn.prepare(
            "SELECT l.path, o.kind, o.ended_at, o.content FROM occurrences o JOIN locations l ON l.id = o.location_id
             WHERE o.epoch_id = ?1 AND o.ino = ?2 AND o.kind IS ?3 AND o.location_id != ?4
             ORDER BY o.id DESC LIMIT 20",
        )?;
        let rows = stmt.query_map(params![epoch, cur.view.ino, cur.view.kind, loc], |r| {
            let ended: Option<i64> = r.get(2)?;
            Ok(Related {
                path: pathenc::encode(&r.get::<_, Vec<u8>>(0)?),
                status: if ended.is_some() { "gone".into() } else { r.get::<_, String>(3)? },
                present: ended.is_none(),
                ended_at: ended,
            })
        })?;
        for r in rows {
            same_inode.push(r?);
        }
    }
    let revisions = match current.as_ref().and_then(|_| occs.iter().find(|b| b.view.present)) {
        Some(b) if b.view.status == "verified" => match b.content_blob_id {
            Some(id) => semantic::revisions_of_blob(conn, id)?,
            None => Vec::new(),
        },
        _ => Vec::new(),
    };
    Ok(LocationDetail {
        path: pathenc::encode(&bytes),
        current,
        occurrences: occs.into_iter().map(|b| b.view).collect(),
        events,
        same_inode,
        revisions,
    })
}

// ----- blobs ----------------------------------------------------------------

#[derive(Serialize)]
pub struct BlobRow {
    pub id: String,
    pub size: i64,
    pub created_at: i64,
    pub format: Option<String>,
    pub present: i64,
    pub paths: Vec<String>,
    pub tags: Vec<AssignmentView>,
    pub lineages: i64,
}

pub fn blobs(cat: &Catalog, filter: &str, q: Option<&str>, limit: i64) -> Result<Vec<BlobRow>> {
    let conn = &cat.conn;
    let epoch = active_epoch(conn)?.unwrap_or(0);
    let present = "(SELECT count(*) FROM occurrences o WHERE o.content_blob_id = b.id AND o.ended_seq IS NULL
                     AND o.epoch_id = ?1 AND o.content IN ('verified', 'plausible'))";
    let cond = match filter {
        "all" | "" => "1".to_string(),
        "duplicates" => format!("{present} >= 2"),
        "missing" => format!("{present} = 0"),
        "tagged" => "EXISTS (SELECT 1 FROM tag_assignments t WHERE t.target_kind = 'blob' AND t.target_id = b.id)".into(),
        "lineage" => "EXISTS (SELECT 1 FROM revisions r WHERE r.blob_id = b.id AND r.tombstoned_at IS NULL)".into(),
        other => return Err(Error::Invalid(format!("unknown blob filter {other:?}"))),
    };
    let qcond = match q.map(str::trim).filter(|q| !q.is_empty()) {
        Some(_) => "AND (b.public_id LIKE ?2 OR EXISTS (SELECT 1 FROM occurrences o JOIN locations l ON l.id = o.location_id
                     WHERE o.content_blob_id = b.id AND o.ended_seq IS NULL AND o.epoch_id = ?1
                       AND lower(CAST(l.path AS TEXT)) LIKE ?2))",
        None => "AND ?2 IS NULL",
    };
    let pattern = q.map(str::trim).filter(|q| !q.is_empty()).map(|q| format!("%{}%", q.to_lowercase()));
    let sql = format!(
        "SELECT b.id, b.public_id, b.size, b.created_at, {present},
                (SELECT payload FROM derived_metadata d WHERE d.blob_id = b.id AND d.extractor = 'sniff'
                 ORDER BY d.id DESC LIMIT 1),
                (SELECT count(DISTINCT r.lineage_id) FROM revisions r WHERE r.blob_id = b.id AND r.tombstoned_at IS NULL)
         FROM blobs b WHERE {cond} {qcond} ORDER BY b.id DESC LIMIT {limit}"
    );
    let mut stmt = conn.prepare(&sql)?;
    #[allow(clippy::type_complexity)]
    let rows: Vec<(i64, String, i64, i64, i64, Option<String>, i64)> = stmt
        .query_map(params![epoch, pattern], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let ids: Vec<i64> = rows.iter().map(|r| r.0).collect();
    let mut tags = semantic::assignments_for(conn, "blob", &ids)?;
    let mut paths: HashMap<i64, Vec<String>> = HashMap::new();
    let mut stmt = conn.prepare(
        "SELECT o.content_blob_id, l.path FROM occurrences o JOIN locations l ON l.id = o.location_id
         WHERE o.content_blob_id IN (SELECT value FROM json_each(?1)) AND o.ended_seq IS NULL AND o.epoch_id = ?2
           AND o.content IN ('verified', 'plausible') ORDER BY l.path",
    )?;
    for row in stmt.query_map(params![serde_json::to_string(&ids)?, epoch], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
    })? {
        let (b, p) = row?;
        paths.entry(b).or_default().push(pathenc::encode(&p));
    }
    Ok(rows
        .into_iter()
        .map(|(id, public, size, created_at, present, fmt, lineages)| BlobRow {
            id: public,
            size,
            created_at,
            format: fmt.as_deref().map(format_label),
            present,
            paths: paths.remove(&id).unwrap_or_default(),
            tags: tags.remove(&id).unwrap_or_default(),
            lineages,
        })
        .collect())
}

#[derive(Serialize)]
pub struct DerivedView {
    pub extractor: String,
    pub version: String,
    pub config: String,
    pub payload: Value,
    pub scan: i64,
    pub created_at: i64,
}

#[derive(Serialize)]
pub struct BlobOccurrence {
    pub path: String,
    pub status: String,
    pub present: bool,
    pub verified_at: Option<i64>,
    pub content_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub end_reason: Option<String>,
}

#[derive(Serialize)]
pub struct BlobDetail {
    pub id: String,
    pub size: i64,
    pub created_at: i64,
    pub admitted_scan: i64,
    pub admitted_path: String,
    pub metadata: Vec<DerivedView>,
    pub baselines: Vec<String>,
    pub occurrences: Vec<BlobOccurrence>,
    pub recognitions: i64,
    pub tags: Vec<AssignmentView>,
    pub revisions: Vec<semantic::RevisionRef>,
}

pub fn blob_detail(cat: &Catalog, public_id: &str) -> Result<BlobDetail> {
    let conn = &cat.conn;
    let (id, size, created_at, scan, path): (i64, i64, i64, i64, Vec<u8>) = conn
        .query_row(
            "SELECT b.id, b.size, b.created_at, r.scan_id, l.path FROM blobs b
             JOIN reads r ON r.id = b.admitted_from_read_id JOIN locations l ON l.id = r.location_id
             WHERE b.public_id = ?1",
            [public_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?
        .ok_or_else(|| Error::NotFound(public_id.to_string()))?;
    let mut metadata = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT extractor, version, config, payload, scan_id, created_at FROM derived_metadata
         WHERE blob_id = ?1 ORDER BY id",
    )?;
    for row in stmt.query_map([id], |r| {
        Ok(DerivedView {
            extractor: r.get(0)?,
            version: r.get(1)?,
            config: r.get(2)?,
            payload: serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or(Value::Null),
            scan: r.get(4)?,
            created_at: r.get(5)?,
        })
    })? {
        metadata.push(row?);
    }
    let mut baselines = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT p.encoding FROM baselines b JOIN sampling_policies p ON p.id = b.policy_id WHERE b.blob_id = ?1",
    )?;
    for row in stmt.query_map([id], |r| r.get::<_, String>(0))? {
        baselines.push(row?);
    }
    let occs = entries(
        cat,
        "(o.content_blob_id = ?1 OR o.verified_blob_id = ?1) ORDER BY (o.ended_seq IS NULL) DESC, l.path",
        &[&id],
        500,
    )?;
    let occurrences = occs
        .into_iter()
        .map(|b| {
            let v = b.view;
            let about_this = v.blob.as_deref() == Some(public_id);
            BlobOccurrence {
                path: v.path,
                status: if about_this { v.status } else { "formerly_verified".into() },
                present: v.present,
                verified_at: v.verified_at,
                content_at: v.content_at,
                ended_at: v.ended_at,
                end_reason: v.end_reason,
            }
        })
        .collect();
    let recognitions: i64 =
        conn.query_row("SELECT count(*) FROM read_results WHERE recognized_blob_id = ?1", [id], |r| r.get(0))?;
    Ok(BlobDetail {
        id: public_id.to_string(),
        size,
        created_at,
        admitted_scan: scan,
        admitted_path: pathenc::encode(&path),
        metadata,
        baselines,
        occurrences,
        recognitions,
        tags: semantic::assignments_for(conn, "blob", &[id])?.remove(&id).unwrap_or_default(),
        revisions: semantic::revisions_of_blob(conn, id)?,
    })
}

// ----- scans ----------------------------------------------------------------

#[derive(Serialize)]
pub struct ScanView {
    pub id: i64,
    pub public_id: String,
    pub operation: String,
    pub targets: Value,
    pub state: String,
    pub outcome: Option<String>,
    pub requested_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub publication_seq: Option<i64>,
    pub summary: Option<Value>,
    pub policy: Option<Value>,
    pub error: Option<String>,
    pub scanner_version: Option<String>,
    pub advisor: Option<String>,
    pub suggestion: Option<String>,
}

const SCAN_SELECT: &str = "SELECT s.id, s.public_id, s.operation, s.targets, s.state, s.outcome, s.requested_at,
        s.started_at, s.finished_at, s.publication_seq, s.summary, s.policy, s.error, s.scanner_version,
        p.advisor, json_extract(p.proposal, '$.title')
    FROM scans s LEFT JOIN action_provenance p ON p.id = s.provenance_id";

fn scan_row(r: &Row) -> rusqlite::Result<ScanView> {
    let json = |v: Option<String>| v.and_then(|s| serde_json::from_str(&s).ok());
    Ok(ScanView {
        id: r.get(0)?,
        public_id: r.get(1)?,
        operation: r.get(2)?,
        targets: json(r.get(3)?).unwrap_or(Value::Null),
        state: r.get(4)?,
        outcome: r.get(5)?,
        requested_at: r.get(6)?,
        started_at: r.get(7)?,
        finished_at: r.get(8)?,
        publication_seq: r.get(9)?,
        summary: json(r.get(10)?),
        policy: json(r.get(11)?),
        error: r.get(12)?,
        scanner_version: r.get(13)?,
        advisor: r.get(14)?,
        suggestion: r.get(15)?,
    })
}

pub fn scans(conn: &Connection, limit: i64) -> Result<Vec<ScanView>> {
    let mut stmt = conn.prepare(&format!("{SCAN_SELECT} ORDER BY s.id DESC LIMIT ?1"))?;
    let rows = stmt.query_map([limit], scan_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[derive(Serialize)]
pub struct DiagView {
    pub at: i64,
    pub severity: String,
    pub code: String,
    pub message: String,
    pub path: Option<String>,
}

#[derive(Serialize)]
pub struct ScanDetail {
    pub scan: ScanView,
    pub diagnostics: Vec<DiagView>,
    pub diagnostics_total: i64,
    pub proposal: Option<Value>,
}

pub fn scan_detail(conn: &Connection, id: i64) -> Result<ScanDetail> {
    let scan = conn
        .query_row(&format!("{SCAN_SELECT} WHERE s.id = ?1"), [id], scan_row)
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("scan #{id}")))?;
    let mut stmt = conn.prepare(
        "SELECT d.at, d.severity, d.code, d.message, l.path FROM diagnostics d
         LEFT JOIN locations l ON l.id = d.location_id WHERE d.scan_id = ?1 ORDER BY d.id LIMIT 1000",
    )?;
    let diagnostics = stmt
        .query_map([id], |r| {
            Ok(DiagView {
                at: r.get(0)?,
                severity: r.get(1)?,
                code: r.get(2)?,
                message: r.get(3)?,
                path: r.get::<_, Option<Vec<u8>>>(4)?.map(|p| pathenc::encode(&p)),
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let diagnostics_total = conn.query_row("SELECT count(*) FROM diagnostics WHERE scan_id = ?1", [id], |r| r.get(0))?;
    let proposal: Option<String> = conn
        .query_row(
            "SELECT p.proposal FROM scans s JOIN action_provenance p ON p.id = s.provenance_id WHERE s.id = ?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(ScanDetail {
        scan,
        diagnostics,
        diagnostics_total,
        proposal: proposal.and_then(|p| serde_json::from_str(&p).ok()),
    })
}

// ----- catalog summary ------------------------------------------------------

#[derive(Serialize, Clone, Debug)]
pub struct EpochView {
    pub id: String,
    pub number: i64,
    pub fs_context: String,
    pub root_ino: i64,
    pub reason: String,
    pub created_at: i64,
}

#[derive(Serialize, Clone, Debug)]
pub struct CatalogInfo {
    pub catalog: String,
    pub root: String,
    pub archive_id: String,
    pub epoch: Option<EpochView>,
    pub epochs: i64,
    pub publication_seq: i64,
    pub pending_root_change: Option<Value>,
    pub verify_horizon_days: i64,
    pub scans: i64,
    pub blobs: i64,
    pub semantic_ops: i64,
}

pub fn info(cat: &Catalog) -> Result<CatalogInfo> {
    let conn = &cat.conn;
    let epoch = conn
        .query_row(
            "SELECT public_id, id, fs_context, root_ino, reason, created_at FROM epochs ORDER BY id DESC LIMIT 1",
            [],
            |r| {
                Ok(EpochView {
                    id: r.get(0)?,
                    number: r.get(1)?,
                    fs_context: r.get(2)?,
                    root_ino: r.get(3)?,
                    reason: r.get(4)?,
                    created_at: r.get(5)?,
                })
            },
        )
        .optional()?;
    let count = |sql: &str| -> Result<i64> { Ok(conn.query_row(sql, [], |r| r.get(0))?) };
    Ok(CatalogInfo {
        catalog: cat.dir().display().to_string(),
        root: cat.root().display().to_string(),
        archive_id: cat.meta("archive_id")?.unwrap_or_default(),
        epoch,
        epochs: count("SELECT count(*) FROM epochs")?,
        publication_seq: cat.publication_seq()?,
        pending_root_change: cat
            .meta("pending_root_change")?
            .and_then(|v| serde_json::from_str(&v).ok()),
        verify_horizon_days: cat.verify_horizon_days(),
        scans: count("SELECT count(*) FROM scans")?,
        blobs: count("SELECT count(*) FROM blobs")?,
        semantic_ops: count("SELECT count(*) FROM semantic_ops")?,
    })
}

pub fn set_horizon(cat: &Catalog, days: i64) -> Result<()> {
    if !(1..=36_500).contains(&days) {
        return Err(Error::Invalid("the verification horizon must be between 1 and 36500 days".into()));
    }
    cat.set_meta("verify_horizon_days", &days.to_string())
}

/// Used by the location view and tests: the JSON shape of a value.
pub fn to_json<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or_else(|e| json!({"error": e.to_string()}))
}
