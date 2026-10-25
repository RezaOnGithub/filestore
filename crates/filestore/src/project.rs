//! Evidence interpretation: the occurrence projection.
//!
//! An occurrence is a derived grouping of observations at one Location with
//! compatible continuity (same identity epoch, same kind, same inode, under
//! the same parent occurrence). Published scans are applied in publication
//! order and, within a scan, in event order. The projection is a cache: it
//! can be rebuilt from published evidence and must come out identical.
//!
//! Transition rules (DESIGN.md §7.3, simplified):
//! * A changed inode or entry type at a location ends the old occurrence
//!   (and every descendant) and starts a new, unidentified one.
//! * A complete, stable, location-confirmed listing ends current children
//!   whose names it omits. Other listings never establish absence.
//! * A directory whose timestamps differ from those recorded at its last
//!   complete listing has a stale listing.
//! * Only a full read with a confirmed location verifies content. Sample and
//!   size comparisons make content plausible or rule the compared Blob out;
//!   a sample match never upgrades or erases a contradiction.
//! * Reads or listings whose location is unconfirmed attach nothing; they
//!   are recorded as an unresolved issue on the current occurrence.
//! * Failures leave earlier evidence in place as last-known, with the
//!   failure visible as an issue.

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension, params};

use crate::{Result, catalog};

pub const INTERPRETATION_VERSION: u32 = 1;

#[derive(Clone, Debug)]
struct Obs {
    id: i64,
    location_id: i64,
    role: String,
    enumeration_id: Option<i64>,
    error: Option<String>,
    kind: Option<String>,
    ino: Option<i64>,
    size: Option<i64>,
    mtime_ns: Option<i64>,
    ctime_ns: Option<i64>,
    mode: Option<i64>,
    boundary: bool,
    observed_at: i64,
    event_order: i64,
}

struct EnumRow {
    location_id: i64,
    dir_obs_id: Option<i64>,
    listing: String,
    stability: String,
    reason: Option<String>,
    entries: i64,
    after_mtime_ns: Option<i64>,
    after_ctime_ns: Option<i64>,
    location_check: String,
    location_reason: Option<String>,
    finished_at: i64,
}

struct ReadRow {
    id: i64,
    location_id: i64,
    opened_obs_id: Option<i64>,
    compared_blob_id: Option<i64>,
    content: String,
    reason: Option<String>,
    recognized_blob_id: Option<i64>,
    location_check: String,
    location_reason: Option<String>,
    finished_at: i64,
}

enum Event {
    Obs(i64),
    Enum(i64),
    Read(i64),
}

#[derive(Debug)]
struct Occ {
    id: i64,
    kind: Option<String>,
    ino: Option<i64>,
    content: String,
    content_blob_id: Option<i64>,
    ev_size: Option<i64>,
    ev_mtime_ns: Option<i64>,
    ev_ctime_ns: Option<i64>,
    listing_enum_id: Option<i64>,
    listing_mtime_ns: Option<i64>,
    listing_ctime_ns: Option<i64>,
}

struct Projector<'a> {
    conn: &'a Connection,
    epoch_id: i64,
    seq: i64,
}

/// Apply one published scan. Must run inside the publication transaction
/// (or a rebuild transaction).
pub fn apply_scan(conn: &Connection, scan_id: i64, seq: i64) -> Result<()> {
    let epoch_id: Option<i64> =
        conn.query_row("SELECT epoch_id FROM scans WHERE id = ?1", [scan_id], |r| r.get(0))?;
    let Some(epoch_id) = epoch_id else {
        return Ok(());
    };

    let mut obs: HashMap<i64, Obs> = HashMap::new();
    let mut entries_by_enum: HashMap<i64, Vec<i64>> = HashMap::new();
    let mut events: Vec<(i64, Event)> = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT id, location_id, role, enumeration_id, error, kind, ino, size, mtime_ns,
                    ctime_ns, mode, boundary, observed_at, event_order
             FROM observations WHERE scan_id = ?1 ORDER BY event_order",
        )?;
        let rows = stmt.query_map([scan_id], |r| {
            Ok(Obs {
                id: r.get(0)?,
                location_id: r.get(1)?,
                role: r.get(2)?,
                enumeration_id: r.get(3)?,
                error: r.get(4)?,
                kind: r.get(5)?,
                ino: r.get(6)?,
                size: r.get(7)?,
                mtime_ns: r.get(8)?,
                ctime_ns: r.get(9)?,
                mode: r.get(10)?,
                boundary: r.get(11)?,
                observed_at: r.get(12)?,
                event_order: r.get(13)?,
            })
        })?;
        for o in rows {
            let o = o?;
            if o.role == "root" || o.role == "anchor" {
                events.push((o.event_order, Event::Obs(o.id)));
            }
            if let Some(e) = o.enumeration_id {
                entries_by_enum.entry(e).or_default().push(o.id);
            }
            obs.insert(o.id, o);
        }
    }

    let mut enums: HashMap<i64, EnumRow> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT e.id, e.start_order, e.location_id, e.dir_obs_id, r.listing, r.stability,
                    r.reason, r.entries, r.after_mtime_ns, r.after_ctime_ns, r.location_check,
                    r.location_reason, r.finished_at
             FROM enumerations e JOIN enumeration_results r ON r.enumeration_id = e.id
             WHERE e.scan_id = ?1",
        )?;
        let rows = stmt.query_map([scan_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                EnumRow {
                    location_id: r.get(2)?,
                    dir_obs_id: r.get(3)?,
                    listing: r.get(4)?,
                    stability: r.get(5)?,
                    reason: r.get(6)?,
                    entries: r.get(7)?,
                    after_mtime_ns: r.get(8)?,
                    after_ctime_ns: r.get(9)?,
                    location_check: r.get(10)?,
                    location_reason: r.get(11)?,
                    finished_at: r.get(12)?,
                },
            ))
        })?;
        for row in rows {
            let (id, order, e) = row?;
            events.push((order, Event::Enum(id)));
            enums.insert(id, e);
        }
    }

    let mut reads: HashMap<i64, ReadRow> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT r.id, r.start_order, r.location_id, r.opened_obs_id, r.compared_blob_id,
                    x.content, x.reason, x.recognized_blob_id, x.location_check, x.location_reason,
                    x.finished_at
             FROM reads r JOIN read_results x ON x.read_id = r.id
             WHERE r.scan_id = ?1",
        )?;
        let rows = stmt.query_map([scan_id], |r| {
            Ok((
                r.get::<_, i64>(1)?,
                ReadRow {
                    id: r.get(0)?,
                    location_id: r.get(2)?,
                    opened_obs_id: r.get(3)?,
                    compared_blob_id: r.get(4)?,
                    content: r.get(5)?,
                    reason: r.get(6)?,
                    recognized_blob_id: r.get(7)?,
                    location_check: r.get(8)?,
                    location_reason: r.get(9)?,
                    finished_at: r.get(10)?,
                },
            ))
        })?;
        for row in rows {
            let (order, rd) = row?;
            events.push((order, Event::Read(rd.id)));
            reads.insert(rd.id, rd);
        }
    }
    events.sort_by_key(|(order, _)| *order);

    let p = Projector { conn, epoch_id, seq };
    for (_, ev) in events {
        match ev {
            Event::Obs(id) => {
                p.apply_obs(&obs[&id])?;
            }
            Event::Enum(id) => {
                let e = &enums[&id];
                let entries: Vec<&Obs> = entries_by_enum
                    .get(&id)
                    .map(|v| v.iter().map(|o| &obs[o]).collect())
                    .unwrap_or_default();
                p.apply_enum(id, e, e.dir_obs_id.map(|d| &obs[&d]), &entries)?;
            }
            Event::Read(id) => {
                let r = &reads[&id];
                p.apply_read(r, r.opened_obs_id.map(|o| &obs[&o]))?;
            }
        }
    }
    catalog::set_meta(conn, "projection_seq", &seq.to_string())?;
    Ok(())
}

/// Discard and recompute the projection from all published scans.
pub fn rebuild(conn: &Connection) -> Result<()> {
    let tx = crate::catalog::atomic(conn)?;
    tx.execute("DELETE FROM occurrences", [])?;
    let scans: Vec<(i64, i64)> = {
        let mut stmt = tx.prepare(
            "SELECT id, publication_seq FROM scans WHERE publication_seq IS NOT NULL
             ORDER BY publication_seq",
        )?;
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    for (id, seq) in scans {
        apply_scan(&tx, id, seq)?;
    }
    tx.commit()?;
    Ok(())
}

/// Every occurrence rendered without row IDs, for comparing projections.
pub fn snapshot(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT o.epoch_id, l.path, pl.path, o.kind, o.ino, o.first_obs_id, o.first_seq,
                o.last_obs_id, o.last_seq, o.size, o.mtime_ns, o.ctime_ns, o.boundary, o.meta_issue,
                o.ended_seq, o.end_reason, o.content, o.content_blob_id, o.content_read_id,
                o.ev_size, o.verified_blob_id, o.verified_read_id, o.check_result, o.check_blob_id,
                o.check_read_id, o.prev_blob_id, o.issue, o.issue_read_id, o.listing_enum_id,
                o.listing_seq, o.listing_stale, o.listing_issue
         FROM occurrences o
         JOIN locations l ON l.id = o.location_id
         LEFT JOIN occurrences po ON po.id = o.parent_occ_id
         LEFT JOIN locations pl ON pl.id = po.location_id
         ORDER BY o.epoch_id, l.path, o.first_seq, o.first_obs_id",
    )?;
    let n = stmt.column_count();
    let rows = stmt.query_map([], |r| {
        let mut parts = Vec::with_capacity(n);
        for i in 0..n {
            let v: rusqlite::types::Value = r.get(i)?;
            parts.push(format!("{v:?}"));
        }
        Ok(parts.join("|"))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

impl Projector<'_> {
    fn current(&self, loc: i64) -> Result<Option<Occ>> {
        Ok(self
            .conn
            .prepare_cached(
                "SELECT id, kind, ino, content, content_blob_id, ev_size, ev_mtime_ns, ev_ctime_ns,
                        listing_enum_id, listing_mtime_ns, listing_ctime_ns
                 FROM occurrences WHERE epoch_id = ?1 AND location_id = ?2 AND ended_seq IS NULL",
            )?
            .query_row(params![self.epoch_id, loc], |r| {
                Ok(Occ {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    ino: r.get(2)?,
                    content: r.get(3)?,
                    content_blob_id: r.get(4)?,
                    ev_size: r.get(5)?,
                    ev_mtime_ns: r.get(6)?,
                    ev_ctime_ns: r.get(7)?,
                    listing_enum_id: r.get(8)?,
                    listing_mtime_ns: r.get(9)?,
                    listing_ctime_ns: r.get(10)?,
                })
            })
            .optional()?)
    }

    fn parent_location(&self, loc: i64) -> Result<Option<i64>> {
        Ok(self
            .conn
            .prepare_cached("SELECT parent_id FROM locations WHERE id = ?1")?
            .query_row([loc], |r| r.get(0))?)
    }

    /// End an occurrence and, transitively, every current descendant.
    fn end(&self, occ_id: i64, at: i64, reason: &str) -> Result<()> {
        self.conn
            .prepare_cached(
                "UPDATE occurrences SET ended_seq = ?2, ended_at = ?3, end_reason = ?4
                 WHERE id = ?1 AND ended_seq IS NULL",
            )?
            .execute(params![occ_id, self.seq, at, reason])?;
        self.conn
            .prepare_cached(
                "WITH RECURSIVE d(id) AS (
                     SELECT id FROM occurrences WHERE parent_occ_id = ?1 AND ended_seq IS NULL
                     UNION ALL
                     SELECT o.id FROM occurrences o JOIN d ON o.parent_occ_id = d.id
                     WHERE o.ended_seq IS NULL)
                 UPDATE occurrences SET ended_seq = ?2, ended_at = ?3, end_reason = 'ancestor_ended'
                 WHERE id IN (SELECT id FROM d)",
            )?
            .execute(params![occ_id, self.seq, at])?;
        Ok(())
    }

    /// Attach an observation to the occurrence at its location, continuing
    /// it when continuity is compatible and starting a new one otherwise.
    /// Returns the occurrence, or `None` when no current parent directory
    /// occurrence exists to attach it to.
    fn apply_obs(&self, o: &Obs) -> Result<Option<i64>> {
        let parent_occ = match self.parent_location(o.location_id)? {
            None => None,
            Some(ploc) => match self.current(ploc)? {
                Some(p) if p.kind.as_deref() == Some("dir") => Some(p.id),
                _ => return Ok(None),
            },
        };
        let cur = self.current(o.location_id)?;
        if o.kind.is_none() {
            // Name seen but metadata unavailable: continuity is unresolved.
            if let Some(c) = cur {
                self.conn
                    .prepare_cached(
                        "UPDATE occurrences SET meta_issue = ?2, last_seq = ?3, last_at = ?4,
                                last_obs_id = ?5 WHERE id = ?1",
                    )?
                    .execute(params![c.id, o.error, self.seq, o.observed_at, o.id])?;
                return Ok(Some(c.id));
            }
            return self.insert_occ(o, parent_occ).map(Some);
        }
        if let Some(c) = &cur {
            if c.kind == o.kind && c.ino.is_some() && c.ino == o.ino {
                let stale = c.kind.as_deref() == Some("dir")
                    && c.listing_enum_id.is_some()
                    && (o.mtime_ns != c.listing_mtime_ns || o.ctime_ns != c.listing_ctime_ns);
                self.conn
                    .prepare_cached(
                        "UPDATE occurrences SET last_obs_id = ?2, last_seq = ?3, last_at = ?4,
                                size = ?5, mtime_ns = ?6, ctime_ns = ?7, mode = ?8, boundary = ?9,
                                meta_issue = NULL,
                                listing_stale = CASE WHEN ?10 THEN 1 ELSE listing_stale END
                         WHERE id = ?1",
                    )?
                    .execute(params![
                        c.id,
                        o.id,
                        self.seq,
                        o.observed_at,
                        o.size,
                        o.mtime_ns,
                        o.ctime_ns,
                        o.mode,
                        o.boundary,
                        stale
                    ])?;
                return Ok(Some(c.id));
            }
            let reason = if c.kind.is_none() || c.ino.is_none() {
                "continuity_unresolved"
            } else if c.kind != o.kind {
                "type_changed"
            } else {
                "replaced"
            };
            self.end(c.id, o.observed_at, reason)?;
        }
        self.insert_occ(o, parent_occ).map(Some)
    }

    fn insert_occ(&self, o: &Obs, parent_occ: Option<i64>) -> Result<i64> {
        let prev_blob: Option<i64> = if o.kind.as_deref() == Some("file") {
            self.conn
                .prepare_cached(
                    "SELECT coalesce(verified_blob_id, prev_blob_id) FROM occurrences
                     WHERE location_id = ?1 AND coalesce(verified_blob_id, prev_blob_id) IS NOT NULL
                     ORDER BY id DESC LIMIT 1",
                )?
                .query_row([o.location_id], |r| r.get(0))
                .optional()?
        } else {
            None
        };
        self.conn
            .prepare_cached(
                "INSERT INTO occurrences
                   (epoch_id, location_id, parent_occ_id, kind, ino, first_obs_id, first_seq, first_at,
                    last_obs_id, last_seq, last_at, size, mtime_ns, ctime_ns, mode, boundary,
                    meta_issue, prev_blob_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            )?
            .execute(params![
                self.epoch_id,
                o.location_id,
                parent_occ,
                o.kind,
                o.ino,
                o.id,
                self.seq,
                o.observed_at,
                o.size,
                o.mtime_ns,
                o.ctime_ns,
                o.mode,
                o.boundary,
                o.error,
                prev_blob
            ])?;
        Ok(self.conn.last_insert_rowid())
    }

    fn set_listing_issue(&self, loc: i64, issue: &str, at: i64) -> Result<()> {
        if let Some(c) = self.current(loc)? {
            self.conn.execute(
                "UPDATE occurrences SET listing_issue = ?2, listing_issue_at = ?3 WHERE id = ?1",
                params![c.id, issue, at],
            )?;
        }
        Ok(())
    }

    fn apply_enum(&self, enum_id: i64, e: &EnumRow, dir_obs: Option<&Obs>, entries: &[&Obs]) -> Result<()> {
        let Some(dir_obs) = dir_obs else {
            let issue = e.reason.as_deref().unwrap_or("listing failed");
            return self.set_listing_issue(e.location_id, issue, e.finished_at);
        };
        if e.location_check != "confirmed" {
            let issue = format!(
                "listing location {}: {}",
                e.location_check,
                e.location_reason.as_deref().unwrap_or("unknown")
            );
            return self.set_listing_issue(e.location_id, &issue, e.finished_at);
        }
        let Some(dir_occ) = self.apply_obs(dir_obs)? else {
            return Ok(());
        };
        for entry in entries {
            self.apply_obs(entry)?;
        }
        if e.listing == "complete" && e.stability == "stable" {
            let seen: HashSet<i64> = entries.iter().map(|o| o.location_id).collect();
            let children: Vec<(i64, i64)> = {
                let mut stmt = self.conn.prepare_cached(
                    "SELECT id, location_id FROM occurrences WHERE parent_occ_id = ?1 AND ended_seq IS NULL",
                )?;
                stmt.query_map([dir_occ], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<_>>()?
            };
            for (child, loc) in children {
                if !seen.contains(&loc) {
                    self.end(child, e.finished_at, "absent_in_listing")?;
                }
            }
            self.conn
                .prepare_cached(
                    "UPDATE occurrences SET listing_enum_id = ?2, listing_at = ?3, listing_seq = ?4,
                            listing_mtime_ns = ?5, listing_ctime_ns = ?6, listing_entries = ?7,
                            listing_stale = 0, listing_issue = NULL, listing_issue_at = NULL
                     WHERE id = ?1",
                )?
                .execute(params![
                    dir_occ,
                    enum_id,
                    e.finished_at,
                    self.seq,
                    e.after_mtime_ns,
                    e.after_ctime_ns,
                    e.entries
                ])?;
        } else {
            let issue = e.reason.clone().unwrap_or_else(|| format!("listing {}", e.listing));
            self.conn.execute(
                "UPDATE occurrences SET listing_issue = ?2, listing_issue_at = ?3,
                        listing_stale = CASE WHEN listing_enum_id IS NULL THEN 0 ELSE 1 END
                 WHERE id = ?1",
                params![dir_occ, issue, e.finished_at],
            )?;
        }
        Ok(())
    }

    fn apply_read(&self, r: &ReadRow, opened: Option<&Obs>) -> Result<()> {
        let failure = matches!(r.content.as_str(), "unreadable" | "unstable" | "integrity_error");
        let Some(opened) = opened.filter(|_| r.location_check == "confirmed") else {
            if let Some(c) = self.current(r.location_id)? {
                let (issue, detail) = if r.opened_obs_id.is_none() {
                    // Nothing was opened: an access failure, not a content one.
                    ("open_failed".to_string(), r.reason.clone())
                } else if failure {
                    (r.content.clone(), r.reason.clone())
                } else {
                    (format!("location_{}", r.location_check), r.location_reason.clone())
                };
                self.set_issue(c.id, &issue, detail.as_deref(), r)?;
            }
            return Ok(());
        };
        let Some(occ_id) = self.apply_obs(opened)? else {
            return Ok(());
        };
        let ev = (opened.size, opened.mtime_ns, opened.ctime_ns);
        match r.content.as_str() {
            "full_recognition" => {
                self.conn
                    .prepare_cached(
                        "UPDATE occurrences SET content = 'verified', content_blob_id = ?2,
                                content_read_id = ?3, content_at = ?4, ev_size = ?5, ev_mtime_ns = ?6,
                                ev_ctime_ns = ?7, verified_blob_id = ?2, verified_read_id = ?3,
                                verified_at = ?4, issue = NULL, issue_detail = NULL, issue_at = NULL,
                                issue_read_id = NULL
                         WHERE id = ?1",
                    )?
                    .execute(params![occ_id, r.recognized_blob_id, r.id, r.finished_at, ev.0, ev.1, ev.2])?;
            }
            "sample_match" | "sample_mismatch" | "size_mismatch" => {
                let occ = self.current(r.location_id)?.expect("just attached");
                let unchanged_verified = r.content == "sample_match"
                    && occ.content == "verified"
                    && occ.content_blob_id == r.compared_blob_id
                    && (occ.ev_size, occ.ev_mtime_ns, occ.ev_ctime_ns) == ev;
                if unchanged_verified {
                    // Agreement with an unmodified verified occurrence: record
                    // the comparison; the verification itself is unchanged.
                    self.set_check(occ_id, r)?;
                } else {
                    let content = if r.content == "sample_match" { "plausible" } else { "changed" };
                    self.conn
                        .prepare_cached(
                            "UPDATE occurrences SET content = ?2, content_blob_id = ?3, content_read_id = ?4,
                                    content_at = ?5, ev_size = ?6, ev_mtime_ns = ?7, ev_ctime_ns = ?8
                             WHERE id = ?1",
                        )?
                        .execute(params![occ_id, content, r.compared_blob_id, r.id, r.finished_at, ev.0, ev.1, ev.2])?;
                    self.set_check(occ_id, r)?;
                }
            }
            "no_baseline" => {
                self.set_check(occ_id, r)?;
            }
            "not_regular" => {}
            _ => {
                self.set_issue(occ_id, &r.content, r.reason.as_deref(), r)?;
            }
        }
        Ok(())
    }

    /// Record a comparison. It resolves only the failures it actually
    /// overcame: a failed open or an unconfirmed location (the object was
    /// opened at a confirmed location), and, when the read passed stability
    /// checks, an unstable read. A mid-read I/O error or an integrity error
    /// stays visible until a full read succeeds: sampled blocks can miss the
    /// failing region.
    fn set_check(&self, occ_id: i64, r: &ReadRow) -> Result<()> {
        let stable_read = r.content != "no_baseline";
        self.conn
            .prepare_cached(
                "UPDATE occurrences SET check_result = ?2, check_blob_id = ?3, check_read_id = ?4, check_at = ?5,
                        issue_read_id = CASE WHEN resolved THEN NULL ELSE issue_read_id END,
                        issue_detail = CASE WHEN resolved THEN NULL ELSE issue_detail END,
                        issue_at = CASE WHEN resolved THEN NULL ELSE issue_at END,
                        issue = CASE WHEN resolved THEN NULL ELSE issue END
                 FROM (SELECT (issue = 'open_failed' OR issue LIKE 'location_%'
                               OR (?6 AND issue = 'unstable')) AS resolved
                       FROM occurrences WHERE id = ?1)
                 WHERE id = ?1",
            )?
            .execute(params![occ_id, r.content, r.compared_blob_id, r.id, r.finished_at, stable_read])?;
        Ok(())
    }

    fn set_issue(&self, occ_id: i64, issue: &str, detail: Option<&str>, r: &ReadRow) -> Result<()> {
        self.conn
            .prepare_cached(
                "UPDATE occurrences SET issue = ?2, issue_detail = ?3, issue_at = ?4, issue_read_id = ?5
                 WHERE id = ?1",
            )?
            .execute(params![occ_id, issue, detail, r.finished_at, r.id])?;
        Ok(())
    }
}
