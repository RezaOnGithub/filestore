//! Advisors: heuristics that read the catalog (never file content) and
//! propose user actions with supporting evidence. Suggestions are computed on
//! demand and not stored; accepting one records the reviewed proposal as
//! provenance, atomically with the scan request or semantic operation.
//!
//! A suggestion's ID is a hash of its advisor and action. Acceptance
//! recomputes the suggestions and requires the same ID to still be offered,
//! so a stale proposal cannot be executed.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};

use rusqlite::{Connection, params};
use serde::Serialize;
use serde_json::Value;

use crate::ids::{fmt_time, now_ms};
use crate::query::{active_epoch, file_status};
use crate::scan::{self, Operation, ScanRef, ScanRequest, Target, TargetKind};
use crate::{Catalog, Error, Result, pathenc};

pub const ADVISOR_VERSION: u32 = 1;
const EVIDENCE_LIMIT: usize = 200;
const DAY_MS: i64 = 86_400_000;
const MAX_SEQUENCE: usize = 20;

#[derive(Serialize, Clone, Debug)]
pub struct Evidence {
    pub path: String,
    pub detail: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct ProposedRevision {
    pub blob: String,
    pub label: Option<String>,
    pub path: String,
}

#[derive(Serialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    Scan {
        operation: Operation,
        targets: Vec<Target>,
    },
    /// Create a Lineage whose revisions form a chain in the given order.
    CreateLineage {
        title: String,
        revisions: Vec<ProposedRevision>,
    },
    /// Add `blob` as a revision whose parent is `parent`.
    AddRevision {
        lineage: String,
        lineage_title: String,
        parent: String,
        parent_label: Option<String>,
        revision: ProposedRevision,
    },
}

#[derive(Serialize, Clone, Debug)]
pub struct Suggestion {
    pub id: String,
    pub advisor: String,
    pub advisor_version: u32,
    pub priority: u32,
    pub title: String,
    pub reason: String,
    pub uncertainty: String,
    pub resolves: String,
    pub action: Action,
    pub evidence: Vec<Evidence>,
    pub evidence_total: usize,
    pub input_seq: i64,
}

struct Cur {
    path: Vec<u8>,
    parent: Option<i64>,
    kind: Option<String>,
    content: String,
    blob: Option<i64>,
    modified_size: bool,
    modified_any: bool,
    issue: Option<String>,
    issue_detail: Option<String>,
    verified_at: Option<i64>,
    listed: bool,
    listing_stale: bool,
    listing_issue: Option<String>,
    boundary: bool,
    ino: Option<i64>,
    first_seq: i64,
    location: i64,
    prev_blob: Option<i64>,
}

struct Ctx<'a> {
    conn: &'a Connection,
    seq: i64,
    horizon_days: i64,
    current: HashMap<i64, Cur>,
    blob_public: HashMap<i64, String>,
}

fn suggestion_id(advisor: &str, action: &Action) -> String {
    let mut h = blake3::Hasher::new();
    h.update(advisor.as_bytes());
    h.update(serde_json::to_string(action).unwrap_or_default().as_bytes());
    format!("sug_{}", &h.finalize().to_hex()[..20])
}

#[allow(clippy::too_many_arguments)]
fn make(
    advisor: &str,
    priority: u32,
    title: String,
    reason: &str,
    uncertainty: &str,
    resolves: &str,
    action: Action,
    mut evidence: Vec<Evidence>,
    seq: i64,
) -> Suggestion {
    let total = evidence.len();
    evidence.truncate(EVIDENCE_LIMIT);
    Suggestion {
        id: suggestion_id(advisor, &action),
        advisor: advisor.into(),
        advisor_version: ADVISOR_VERSION,
        priority,
        title,
        reason: reason.into(),
        uncertainty: uncertainty.into(),
        resolves: resolves.into(),
        action,
        evidence,
        evidence_total: total,
        input_seq: seq,
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

pub fn suggestions(cat: &Catalog) -> Result<Vec<Suggestion>> {
    let conn = &cat.conn;
    let Some(epoch) = active_epoch(conn)? else {
        return Ok(vec![make(
            "first-scan",
            0,
            "Inventory the whole root".into(),
            "Nothing has been scanned yet.",
            "Inventory reads names and metadata only; no content is identified.",
            "Establishes the hierarchy so content can be verified selectively.",
            Action::Scan {
                operation: Operation::Inventory,
                targets: vec![Target {
                    path: String::new(),
                    kind: TargetKind::Dir,
                    depth: None,
                }],
            },
            Vec::new(),
            0,
        )]);
    };
    let mut ctx = Ctx {
        conn,
        seq: cat.publication_seq()?,
        horizon_days: cat.verify_horizon_days(),
        current: HashMap::new(),
        blob_public: HashMap::new(),
    };
    let mut stmt = conn.prepare(
        "SELECT o.id, l.path, o.parent_occ_id, o.kind, o.content, o.content_blob_id, o.size, o.mtime_ns,
                o.ctime_ns, o.ev_size, o.ev_mtime_ns, o.ev_ctime_ns, o.issue, o.issue_detail, o.verified_at,
                o.listing_enum_id IS NOT NULL, o.listing_stale, o.listing_issue, o.boundary, o.ino, o.first_seq,
                o.location_id, o.prev_blob_id
         FROM occurrences o JOIN locations l ON l.id = o.location_id
         WHERE o.epoch_id = ?1 AND o.ended_seq IS NULL",
    )?;
    let mut rows = stmt.query([epoch])?;
    while let Some(r) = rows.next()? {
        let size: Option<i64> = r.get(6)?;
        let ev_size: Option<i64> = r.get(9)?;
        let content: String = r.get(4)?;
        let has_ev = content == "verified" || content == "plausible";
        let modified_size = has_ev && size != ev_size;
        let modified_any = has_ev
            && (modified_size
                || r.get::<_, Option<i64>>(7)? != r.get::<_, Option<i64>>(10)?
                || r.get::<_, Option<i64>>(8)? != r.get::<_, Option<i64>>(11)?);
        ctx.current.insert(
            r.get(0)?,
            Cur {
                path: r.get(1)?,
                parent: r.get(2)?,
                kind: r.get(3)?,
                content,
                blob: r.get(5)?,
                modified_size,
                modified_any,
                issue: r.get(12)?,
                issue_detail: r.get(13)?,
                verified_at: r.get(14)?,
                listed: r.get(15)?,
                listing_stale: r.get(16)?,
                listing_issue: r.get(17)?,
                boundary: r.get(18)?,
                ino: r.get(19)?,
                first_seq: r.get(20)?,
                location: r.get(21)?,
                prev_blob: r.get(22)?,
            },
        );
    }
    drop(rows);
    drop(stmt);
    let mut stmt = conn.prepare("SELECT id, public_id FROM blobs")?;
    for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
        let (id, p) = row?;
        ctx.blob_public.insert(id, p);
    }

    let mut out = Vec::new();
    let (moves, moved_files, moved_dirs) = possible_moves(&ctx, epoch)?;
    out.extend(moves);
    out.extend(read_failures(&ctx));
    out.extend(changed_content(&ctx));
    out.extend(modified_files(&ctx));
    out.extend(listing_gaps(&ctx, epoch, &moved_dirs)?);
    out.extend(unidentified(&ctx, &moved_files));
    out.extend(plausible_only(&ctx));
    out.extend(overdue(&ctx));
    out.extend(lineage_successors(&ctx)?);
    out.extend(name_sequences(&ctx)?);
    out.sort_by_key(|s| s.priority);
    Ok(out)
}

pub fn find(cat: &Catalog, id: &str) -> Result<Suggestion> {
    suggestions(cat)?.into_iter().find(|s| s.id == id).ok_or_else(|| {
        Error::Conflict(
            "this suggestion is no longer offered: the catalog changed since it was computed; review the current suggestions".into(),
        )
    })
}

pub fn record_provenance(conn: &Connection, s: &Suggestion, modifications: Option<Value>) -> Result<i64> {
    let mut proposal = serde_json::to_value(s)?;
    if let Some(m) = modifications {
        proposal["user_modifications"] = m;
    }
    conn.execute(
        "INSERT INTO action_provenance (advisor, advisor_version, suggestion_id, input_seq, proposal, accepted_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![s.advisor, s.advisor_version, s.id, s.input_seq, proposal.to_string(), now_ms()],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Accept a scan suggestion: the queued request and its provenance are
/// recorded in one transaction.
pub fn accept_scan(cat: &Catalog, id: &str) -> Result<ScanRef> {
    let s = find(cat, id)?;
    let Action::Scan { operation, targets } = &s.action else {
        return Err(Error::Invalid("this suggestion is a semantic change; review and accept it from its dialog".into()));
    };
    let tx = crate::catalog::atomic(&cat.conn)?;
    let prov = record_provenance(&tx, &s, None)?;
    let r = scan::submit(
        &tx,
        &ScanRequest {
            operation: *operation,
            targets: targets.clone(),
        },
        Some(prov),
    )?;
    tx.commit()?;
    Ok(r)
}

impl Ctx<'_> {
    fn blob(&self, id: Option<i64>) -> String {
        id.and_then(|i| self.blob_public.get(&i).cloned()).unwrap_or_else(|| "?".into())
    }

    /// Verify/Check targets for a set of file occurrences: a directory with
    /// depth 0 when every current file in it is included, else the files.
    fn compact(&self, files: &[i64]) -> Vec<Target> {
        let mut by_dir: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
        for f in files {
            if let Some(p) = self.current[f].parent {
                by_dir.entry(p).or_default().push(*f);
            }
        }
        let mut files_in: HashMap<i64, usize> = HashMap::new();
        for c in self.current.values() {
            if c.kind.as_deref() == Some("file")
                && !c.boundary
                && let Some(p) = c.parent
            {
                *files_in.entry(p).or_default() += 1;
            }
        }
        let mut targets = Vec::new();
        for (dir, members) in by_dir {
            if members.len() >= 2 && files_in.get(&dir) == Some(&members.len()) {
                targets.push(Target {
                    path: pathenc::encode(&self.current[&dir].path),
                    kind: TargetKind::Dir,
                    depth: Some(0),
                });
            } else {
                for f in members {
                    targets.push(Target {
                        path: pathenc::encode(&self.current[&f].path),
                        kind: TargetKind::File,
                        depth: None,
                    });
                }
            }
        }
        targets
    }

    fn files_where(&self, pred: impl Fn(&Cur) -> bool) -> Vec<i64> {
        let mut v: Vec<i64> = self
            .current
            .iter()
            .filter(|(_, c)| c.kind.as_deref() == Some("file") && !c.boundary && pred(c))
            .map(|(id, _)| *id)
            .collect();
        v.sort_by(|a, b| self.current[a].path.cmp(&self.current[b].path));
        v
    }

    fn evidence(&self, ids: &[i64], detail: impl Fn(&Cur) -> String) -> Vec<Evidence> {
        ids.iter()
            .map(|id| {
                let c = &self.current[id];
                Evidence {
                    path: pathenc::encode(&c.path),
                    detail: detail(c),
                }
            })
            .collect()
    }
}

fn read_failures(ctx: &Ctx) -> Option<Suggestion> {
    let ids = ctx.files_where(|c| c.issue.is_some());
    if ids.is_empty() {
        return None;
    }
    let evidence = ctx.evidence(&ids, |c| {
        format!(
            "{}{}",
            c.issue.as_deref().unwrap_or(""),
            c.issue_detail.as_deref().map(|d| format!(": {d}")).unwrap_or_default()
        )
    });
    let causes: std::collections::BTreeSet<&str> = ids
        .iter()
        .filter_map(|id| ctx.current[id].issue_detail.as_deref())
        .filter_map(|d| ["EACCES", "EPERM", "EIO", "ENOENT"].into_iter().find(|c| d.contains(c)))
        .collect();
    let causes = if causes.is_empty() {
        String::new()
    } else {
        format!(" ({})", causes.into_iter().collect::<Vec<_>>().join(", "))
    };
    Some(make(
        "read-failures",
        3,
        format!("Retry {}{causes}", plural(ids.len(), "file whose last read failed", "files whose last read failed")),
        "The most recent read of these files was unreadable, unstable, or could not confirm the file's location.",
        "Earlier evidence is still shown as last-known; an access failure says nothing about the content. Fix permissions first if the cause is EACCES.",
        "Re-establishes the current content and location of each file.",
        Action::Scan {
            operation: Operation::Verify,
            targets: ctx.compact(&ids),
        },
        evidence,
        ctx.seq,
    ))
}

fn changed_content(ctx: &Ctx) -> Option<Suggestion> {
    let ids = ctx.files_where(|c| c.content == "changed" && c.issue.is_none());
    if ids.is_empty() {
        return None;
    }
    let evidence = ctx.evidence(&ids, |c| format!("ruled out {}", ctx.blob(c.blob)));
    Some(make(
        "changed-content",
        1,
        format!("Verify {}", plural(ids.len(), "file whose content changed", "files whose content changed")),
        "A stable, location-confirmed size or sample comparison ruled out the previously recorded content at these locations.",
        "Their current content is unidentified. The earlier Blobs keep their tags and revisions.",
        "Identifies the new content (possibly suggesting a new revision).",
        Action::Scan {
            operation: Operation::Verify,
            targets: ctx.compact(&ids),
        },
        evidence,
        ctx.seq,
    ))
}

fn modified_files(ctx: &Ctx) -> Vec<Suggestion> {
    let mut out = Vec::new();
    let sized = ctx.files_where(|c| c.modified_size && c.issue.is_none());
    if !sized.is_empty() {
        let evidence = ctx.evidence(&sized, |c| format!("size differs from when {} was established", ctx.blob(c.blob)));
        out.push(make(
            "modified-files",
            1,
            format!("Verify {}", plural(sized.len(), "file whose size changed", "files whose size changed")),
            "Inventory saw a different size than the read that established their content.",
            "The size came from a directory listing, not a stable handle, so it is a strong hint rather than proof.",
            "Identifies the current content.",
            Action::Scan {
                operation: Operation::Verify,
                targets: ctx.compact(&sized),
            },
            evidence,
            ctx.seq,
        ));
    }
    let touched = ctx.files_where(|c| c.modified_any && !c.modified_size && c.issue.is_none());
    if !touched.is_empty() {
        let evidence = ctx.evidence(&touched, |c| format!("timestamps differ since {} was established", ctx.blob(c.blob)));
        out.push(make(
            "modified-files",
            2,
            format!("Check {}", plural(touched.len(), "file modified since its content was established", "files modified since their content was established")),
            "Their modification or change time differs from the read that established their content. Metadata alone does not prove a content change.",
            "A sample match will only make the expected content plausible; changes outside the sampled blocks can be missed.",
            "Detects most content changes cheaply; mismatches lead to a Verify suggestion.",
            Action::Scan {
                operation: Operation::Check,
                targets: ctx.compact(&touched),
            },
            evidence,
            ctx.seq,
        ));
    }
    out
}

fn listing_gaps(ctx: &Ctx, epoch: i64, exclude: &HashSet<i64>) -> Result<Vec<Suggestion>> {
    let mut replaced: HashSet<i64> = HashSet::new();
    let mut stmt = ctx.conn.prepare(
        "SELECT DISTINCT location_id FROM occurrences
         WHERE epoch_id = ?1 AND end_reason IN ('replaced', 'type_changed', 'continuity_unresolved')",
    )?;
    for row in stmt.query_map([epoch], |r| r.get::<_, i64>(0))? {
        replaced.insert(row?);
    }
    let mut unknown: Vec<(i64, String)> = Vec::new();
    let mut stale: Vec<(i64, String)> = Vec::new();
    for (id, c) in &ctx.current {
        if c.kind.as_deref() != Some("dir") || c.boundary || exclude.contains(id) {
            continue;
        }
        if let Some(issue) = &c.listing_issue {
            stale.push((*id, format!("last listing attempt failed: {issue}")));
        } else if !c.listed {
            let why = if replaced.contains(&c.location) {
                "a different directory now occupies this path; its entries are unknown"
            } else {
                "never listed"
            };
            unknown.push((*id, why.into()));
        } else if c.listing_stale {
            stale.push((*id, "the directory changed since its last complete listing".into()));
        }
    }
    let mut out = Vec::new();
    for (list, depth, title, reason, priority) in [
        (
            &mut unknown,
            None,
            "whose entries are unknown",
            "These directories were seen but never listed (or were replaced), so their contents are unknown, not empty.",
            1,
        ),
        (
            &mut stale,
            Some(0),
            "whose listings are stale or failed",
            "These directories changed since their last complete listing, or the last attempt failed; their current names are not established.",
            1,
        ),
    ] {
        if list.is_empty() {
            continue;
        }
        list.sort_by(|a, b| ctx.current[&a.0].path.cmp(&ctx.current[&b.0].path));
        let targets: Vec<Target> = list
            .iter()
            .map(|(id, _)| Target {
                path: pathenc::encode(&ctx.current[id].path),
                kind: TargetKind::Dir,
                depth,
            })
            .collect();
        let targets = scan::normalize_targets(&targets).unwrap_or(targets);
        let evidence: Vec<Evidence> = list
            .iter()
            .map(|(id, d)| Evidence {
                path: pathenc::encode(&ctx.current[id].path),
                detail: d.clone(),
            })
            .collect();
        out.push(make(
            "listing-gaps",
            priority,
            format!("Inventory {} {title}", plural(list.len(), "directory", "directories")),
            reason,
            "Inventory reads names and metadata only; files found there will still need Verify.",
            "Establishes complete listing coverage for these directories.",
            Action::Scan {
                operation: Operation::Inventory,
                targets,
            },
            evidence,
            ctx.seq,
        ));
    }
    Ok(out)
}

fn unidentified(ctx: &Ctx, exclude: &HashSet<i64>) -> Option<Suggestion> {
    let ids = ctx.files_where(|c| c.content == "unidentified" && c.issue.is_none());
    let ids: Vec<i64> = ids.into_iter().filter(|i| !exclude.contains(i)).collect();
    if ids.is_empty() {
        return None;
    }
    let evidence = ctx.evidence(&ids, |c| match c.prev_blob {
        Some(p) => format!("never read since it appeared; {} was verified at this path before", ctx.blob(Some(p))),
        None => "never fully read at this location".into(),
    });
    Some(make(
        "unidentified-files",
        3,
        format!("Verify {}", plural(ids.len(), "unidentified file", "unidentified files")),
        "These files were inventoried but have never been fully read at their current location, so their content is unknown.",
        "Until verified they cannot be tagged or used as revisions. Large files take time to hash.",
        "Exact content identity (Blob) and verified presence at these locations.",
        Action::Scan {
            operation: Operation::Verify,
            targets: ctx.compact(&ids),
        },
        evidence,
        ctx.seq,
    ))
}

fn plausible_only(ctx: &Ctx) -> Option<Suggestion> {
    let ids = ctx.files_where(|c| file_status(&c.content, c.modified_any) == "plausible" && c.issue.is_none());
    if ids.is_empty() {
        return None;
    }
    let evidence = ctx.evidence(&ids, |c| match c.verified_at {
        Some(_) => format!("sampled as {} after a metadata change", ctx.blob(c.blob)),
        None => format!("sampled as {}; never fully read at this occurrence", ctx.blob(c.blob)),
    });
    Some(make(
        "plausible-only",
        4,
        format!("Verify {} known only by sample match", plural(ids.len(), "file", "files")),
        "Their content matched the sampled blocks of previously verified content, for example after a restore, a replacement, or a metadata change, but has not been fully read since.",
        "Plausible is not verified: changes outside the sampled blocks would be missed. This is optional when that risk is acceptable.",
        "Exact verification at these locations.",
        Action::Scan {
            operation: Operation::Verify,
            targets: ctx.compact(&ids),
        },
        evidence,
        ctx.seq,
    ))
}

fn overdue(ctx: &Ctx) -> Option<Suggestion> {
    let cutoff = now_ms() - ctx.horizon_days * DAY_MS;
    let ids = ctx.files_where(|c| {
        file_status(&c.content, c.modified_any) == "verified" && c.issue.is_none() && c.verified_at.is_some_and(|v| v < cutoff)
    });
    if ids.is_empty() {
        return None;
    }
    let evidence = ctx.evidence(&ids, |c| format!("last verified as {}", ctx.blob(c.blob)));
    Some(make(
        "overdue",
        4,
        format!(
            "Re-verify {} last verified more than {} days ago",
            plural(ids.len(), "file", "files"),
            ctx.horizon_days
        ),
        "Their last verification is older than the verification horizon. Overdue does not mean absent or corrupt.",
        "Sample checks since then do not renew a verification.",
        "Renews exact verification at these locations.",
        Action::Scan {
            operation: Operation::Verify,
            targets: ctx.compact(&ids),
        },
        evidence,
        ctx.seq,
    ))
}

/// Ended occurrences whose inode now appears at another location.
fn possible_moves(ctx: &Ctx, epoch: i64) -> Result<(Vec<Suggestion>, HashSet<i64>, HashSet<i64>)> {
    let by_ino: HashMap<(i64, &str), Vec<i64>> = {
        let mut m: HashMap<(i64, &str), Vec<i64>> = HashMap::new();
        for (id, c) in &ctx.current {
            if let (Some(ino), Some(kind)) = (c.ino, c.kind.as_deref()) {
                m.entry((ino, kind)).or_default().push(*id);
            }
        }
        m
    };
    let mut stmt = ctx.conn.prepare(
        "SELECT l.path, o.kind, o.ino, o.ended_seq, o.verified_blob_id, o.location_id
         FROM occurrences o JOIN locations l ON l.id = o.location_id
         WHERE o.epoch_id = ?1 AND o.end_reason IN ('absent_in_listing', 'ancestor_ended') AND o.ino IS NOT NULL
         ORDER BY o.id DESC",
    )?;
    #[allow(clippy::type_complexity)]
    let rows: Vec<(Vec<u8>, String, i64, i64, Option<i64>, i64)> = stmt
        .query_map([epoch], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut files: Vec<(i64, Evidence)> = Vec::new();
    let mut dirs: Vec<(i64, Evidence)> = Vec::new();
    let mut seen: HashSet<i64> = HashSet::new();
    for (old_path, kind, ino, ended_seq, blob, old_loc) in rows {
        let Some(cands) = by_ino.get(&(ino, kind.as_str())) else { continue };
        for cand in cands {
            let c = &ctx.current[cand];
            if c.location == old_loc || c.first_seq < ended_seq || !seen.insert(*cand) {
                continue;
            }
            let old = pathenc::encode(&old_path);
            if kind == "file" && c.content == "unidentified" && c.issue.is_none() {
                let was = blob.map(|b| format!("; it was verified there as {}", ctx.blob(Some(b)))).unwrap_or_default();
                files.push((
                    *cand,
                    Evidence {
                        path: pathenc::encode(&c.path),
                        detail: format!("may be the former {old} (same inode {ino}){was}"),
                    },
                ));
            } else if kind == "dir" && !c.listed && !c.boundary {
                dirs.push((
                    *cand,
                    Evidence {
                        path: pathenc::encode(&c.path),
                        detail: format!("may be the former {old} (same inode {ino})"),
                    },
                ));
            }
        }
    }
    let mut out = Vec::new();
    let moved_files: HashSet<i64> = files.iter().map(|f| f.0).collect();
    let moved_dirs: HashSet<i64> = dirs.iter().map(|d| d.0).collect();
    if !files.is_empty() {
        let ids: Vec<i64> = files.iter().map(|f| f.0).collect();
        out.push(make(
            "possible-moves",
            2,
            format!("Verify {} that may have moved", plural(files.len(), "file", "files")),
            "A file disappeared from a complete listing while an object with the same native identity appeared elsewhere.",
            "A matching inode suggests a move but establishes neither content nor continuity; nothing is transferred until the new location is verified.",
            "Verification recovers the existing Blob (with its tags and revisions) if the bytes are unchanged.",
            Action::Scan {
                operation: Operation::Verify,
                targets: ids
                    .iter()
                    .map(|id| Target {
                        path: pathenc::encode(&ctx.current[id].path),
                        kind: TargetKind::File,
                        depth: None,
                    })
                    .collect(),
            },
            files.into_iter().map(|f| f.1).collect(),
            ctx.seq,
        ));
    }
    if !dirs.is_empty() {
        out.push(make(
            "possible-moves",
            1,
            format!("Inventory {} that may have been renamed", plural(dirs.len(), "directory", "directories")),
            "A directory disappeared from a complete listing while a directory with the same native identity appeared elsewhere.",
            "The old listing and descendants are not transplanted to the new path; it needs fresh enumeration.",
            "Establishes the contents of the new location.",
            Action::Scan {
                operation: Operation::Inventory,
                targets: dirs
                    .iter()
                    .map(|d| Target {
                        path: pathenc::encode(&ctx.current[&d.0].path),
                        kind: TargetKind::Dir,
                        depth: None,
                    })
                    .collect(),
            },
            dirs.into_iter().map(|d| d.1).collect(),
            ctx.seq,
        ));
    }
    Ok((out, moved_files, moved_dirs))
}

struct RevInfo {
    revision: String,
    label: Option<String>,
    lineage: String,
    lineage_title: String,
}

/// Latest live revision per Blob, with any other Lineages containing it.
fn revisions_by_blob(conn: &Connection) -> Result<HashMap<i64, Vec<RevInfo>>> {
    let mut out: HashMap<i64, Vec<RevInfo>> = HashMap::new();
    let mut stmt = conn.prepare(
        "SELECT r.blob_id, r.public_id, r.label, l.public_id, l.title FROM revisions r
         JOIN lineages l ON l.id = r.lineage_id
         WHERE r.tombstoned_at IS NULL AND l.tombstoned_at IS NULL ORDER BY r.id DESC",
    )?;
    for row in stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            RevInfo {
                revision: r.get(1)?,
                label: r.get(2)?,
                lineage: r.get(3)?,
                lineage_title: r.get(4)?,
            },
        ))
    })? {
        let (b, info) = row?;
        out.entry(b).or_default().push(info);
    }
    Ok(out)
}

fn display_name(path: &[u8]) -> String {
    pathenc::display(pathenc::file_name(path))
}

fn stem(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i > 0 && name.len() - i <= 6 => &name[..i],
        _ => name,
    }
}

/// A location verified as A and later as B: propose B as a revision after A.
fn lineage_successors(ctx: &Ctx) -> Result<Vec<Suggestion>> {
    let conn = ctx.conn;
    let mut stmt = conn.prepare(
        "SELECT r.location_id, x.recognized_blob_id, x.finished_at
         FROM reads r JOIN read_results x ON x.read_id = r.id JOIN scans s ON s.id = r.scan_id
         WHERE x.content = 'full_recognition' AND x.location_check = 'confirmed' AND s.publication_seq IS NOT NULL
           AND r.location_id IN (
               SELECT r2.location_id FROM reads r2 JOIN read_results x2 ON x2.read_id = r2.id
               WHERE x2.content = 'full_recognition' AND x2.location_check = 'confirmed'
               GROUP BY r2.location_id HAVING count(DISTINCT x2.recognized_blob_id) >= 2)
         ORDER BY s.publication_seq, x.end_order",
    )?;
    let mut history: BTreeMap<i64, Vec<(i64, i64)>> = BTreeMap::new();
    for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)))? {
        let (loc, blob, at) = row?;
        let h = history.entry(loc).or_default();
        if h.last().map(|l| l.0) != Some(blob) {
            h.push((blob, at));
        }
    }
    if history.is_empty() {
        return Ok(Vec::new());
    }
    let revs = revisions_by_blob(conn)?;
    let mut parents_of: HashMap<i64, HashSet<i64>> = HashMap::new();
    let mut stmt = conn.prepare(
        "SELECT c.blob_id, p.blob_id FROM revision_parents rp
         JOIN revisions c ON c.id = rp.revision_id JOIN revisions p ON p.id = rp.parent_id
         WHERE c.tombstoned_at IS NULL",
    )?;
    for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
        let (c, p) = row?;
        parents_of.entry(c).or_default().insert(p);
    }
    let by_location: HashMap<i64, &Cur> = ctx.current.values().map(|c| (c.location, c)).collect();
    let mut out = Vec::new();
    for (loc, h) in history {
        if h.len() < 2 {
            continue;
        }
        let (b, b_at) = h[h.len() - 1];
        let (a, a_at) = h[h.len() - 2];
        let Some(cur) = by_location.get(&loc) else { continue };
        if file_status(&cur.content, cur.modified_any) != "verified" || cur.blob != Some(b) {
            continue;
        }
        if parents_of.get(&b).is_some_and(|ps| ps.contains(&a)) {
            continue;
        }
        let name = display_name(&cur.path);
        let path = pathenc::encode(&cur.path);
        let evidence = vec![Evidence {
            path: path.clone(),
            detail: format!(
                "verified as {} at {}, later as {} at {}",
                ctx.blob(Some(a)),
                fmt_time(a_at),
                ctx.blob(Some(b)),
                fmt_time(b_at)
            ),
        }];
        let label = |at: i64| Some(format!("as verified {}", fmt_time(at).trim_end_matches(" UTC")));
        let new_rev = ProposedRevision {
            blob: ctx.blob(Some(b)),
            label: label(b_at),
            path: path.clone(),
        };
        let (action, uncertainty) = match revs.get(&a).and_then(|v| v.first()) {
            Some(r) => {
                let others = revs[&a].len() - 1;
                (
                    Action::AddRevision {
                        lineage: r.lineage.clone(),
                        lineage_title: r.lineage_title.clone(),
                        parent: r.revision.clone(),
                        parent_label: r.label.clone(),
                        revision: new_rev,
                    },
                    if others > 0 {
                        format!("The earlier content also appears in {others} other revision(s); choose the relevant Lineage before accepting.")
                    } else {
                        "Same path does not imply the same document; review before recording the relationship.".into()
                    },
                )
            }
            None => (
                Action::CreateLineage {
                    title: stem(&name).to_string(),
                    revisions: vec![
                        ProposedRevision {
                            blob: ctx.blob(Some(a)),
                            label: label(a_at),
                            path: path.clone(),
                        },
                        new_rev,
                    ],
                },
                "Same path does not imply the same document; review before recording the relationship.".into(),
            ),
        };
        out.push(make(
            "lineage-successor",
            5,
            format!("Review recording the new content of {name} as a later revision"),
            "This location was verified with one content and later with different content.",
            &uncertainty,
            "Records that the newer Blob is a revision of the older one; tags stay on their Blobs.",
            action,
            evidence,
            ctx.seq,
        ));
    }
    out.truncate(50);
    Ok(out)
}

/// Group key for filenames that differ only in digits or bracketed parts.
pub fn sequence_key(name: &str) -> Option<String> {
    let (base, ext) = match name.rfind('.') {
        Some(i) if i > 0 && name.len() - i <= 6 && name[i + 1..].chars().all(|c| c.is_ascii_alphanumeric()) => {
            (&name[..i], name[i + 1..].to_ascii_lowercase())
        }
        _ => (name, String::new()),
    };
    let mut depth = 0usize;
    let mut kept = String::new();
    for c in base.chars() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            _ if depth == 0 => kept.push(c),
            _ => {}
        }
    }
    let mut key = String::new();
    let mut prev_digit = false;
    for c in kept.to_lowercase().chars() {
        if c.is_ascii_digit() {
            if !prev_digit {
                key.push('#');
            }
            prev_digit = true;
        } else {
            prev_digit = false;
            key.push(if c.is_whitespace() { ' ' } else { c });
        }
    }
    let key: String = key.split_whitespace().collect::<Vec<_>>().join(" ");
    let key = key.trim_matches(|c: char| c == '-' || c == '_' || c == '.' || c == ',' || c == ' ');
    // Generic stems such as `IMG_#`, `DSC#`, or `file#` mark series, not versions.
    if key.chars().filter(|c| c.is_alphabetic()).count() < 6 {
        return None;
    }
    Some(format!("{key}.{ext}"))
}

/// Natural ordering: digit runs compare numerically.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    fn runs(s: &str) -> Vec<(bool, &str)> {
        let mut out = Vec::new();
        let mut start = 0;
        let bytes = s.as_bytes();
        for i in 1..=bytes.len() {
            if i == bytes.len() || bytes[i].is_ascii_digit() != bytes[start].is_ascii_digit() {
                out.push((bytes[start].is_ascii_digit(), &s[start..i]));
                start = i;
            }
        }
        out
    }
    let (ra, rb) = (runs(a), runs(b));
    for (x, y) in ra.iter().zip(&rb) {
        let o = match (x.0, y.0) {
            (true, true) => {
                let (tx, ty) = (x.1.trim_start_matches('0'), y.1.trim_start_matches('0'));
                tx.len().cmp(&ty.len()).then_with(|| tx.cmp(ty))
            }
            _ => x.1.to_lowercase().cmp(&y.1.to_lowercase()),
        };
        if o != Ordering::Equal {
            return o;
        }
    }
    ra.len().cmp(&rb.len())
}

fn name_sequences(ctx: &Ctx) -> Result<Vec<Suggestion>> {
    let revs = revisions_by_blob(ctx.conn)?;
    let mut groups: BTreeMap<(i64, String), Vec<i64>> = BTreeMap::new();
    for (id, c) in &ctx.current {
        if c.kind.as_deref() != Some("file") || file_status(&c.content, c.modified_any) != "verified" {
            continue;
        }
        let Some(parent) = c.parent else { continue };
        let name = display_name(&c.path);
        if let Some(key) = sequence_key(&name) {
            groups.entry((parent, key)).or_default().push(*id);
        }
    }
    let mut out = Vec::new();
    for ((_, _), mut members) in groups {
        let blobs: HashSet<Option<i64>> = members.iter().map(|m| ctx.current[m].blob).collect();
        // Large groups are numbered series (scans, photos, chapters), not versions.
        if blobs.len() < 2
            || blobs.len() > MAX_SEQUENCE
            || members.iter().any(|m| ctx.current[m].blob.is_some_and(|b| revs.contains_key(&b)))
        {
            continue;
        }
        members.sort_by(|a, b| natural_cmp(&display_name(&ctx.current[a].path), &display_name(&ctx.current[b].path)));
        // One revision per distinct Blob, in name order.
        let mut seen = HashSet::new();
        members.retain(|m| seen.insert(ctx.current[m].blob));
        let first = display_name(&ctx.current[&members[0]].path);
        let title = {
            let mut depth = 0usize;
            let t: String = stem(&first)
                .chars()
                .filter(|c| {
                    match c {
                        '(' | '[' => depth += 1,
                        ')' | ']' => {
                            depth = depth.saturating_sub(1);
                            return false;
                        }
                        _ => {}
                    }
                    depth == 0
                })
                .collect();
            t.trim().trim_end_matches(['-', ',', ' ']).trim().to_string()
        };
        let dir = pathenc::parent(&ctx.current[&members[0]].path).map(pathenc::display).unwrap_or_default();
        let revisions: Vec<ProposedRevision> = members
            .iter()
            .map(|m| {
                let c = &ctx.current[m];
                ProposedRevision {
                    blob: ctx.blob(c.blob),
                    label: Some(stem(&display_name(&c.path)).to_string()),
                    path: pathenc::encode(&c.path),
                }
            })
            .collect();
        let evidence = members
            .iter()
            .map(|m| {
                let c = &ctx.current[m];
                Evidence {
                    path: pathenc::encode(&c.path),
                    detail: format!("verified as {}", ctx.blob(c.blob)),
                }
            })
            .collect();
        out.push(make(
            "name-sequence",
            6,
            format!("Review a possible revision sequence: {title} ({} files)", members.len()),
            &format!(
                "These verified files in {} have names that differ only in numbers or bracketed parts, which often marks versions of one document.",
                if dir.is_empty() { "the root directory".to_string() } else { dir }
            ),
            "A filename heuristic: they may be separate volumes or parts rather than revisions. The order is a natural sort of the names.",
            "Records the files' Blobs as a chain of revisions in a new Lineage.",
            Action::CreateLineage { title, revisions },
            evidence,
            ctx.seq,
        ));
    }
    out.truncate(100);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_keys() {
        let a = sequence_key("5-Level Paging and 5-Level EPT - Intel - Revision 1.0 (December, 2016).pdf");
        let b = sequence_key("5-Level Paging and 5-Level EPT - Intel - Revision 1.1 (May, 2017).pdf");
        assert_eq!(a, b);
        assert!(a.is_some());
        assert_eq!(
            sequence_key("Proof of Concept or GTFO - v13 (October 18th, 2016).pdf"),
            sequence_key("Proof of Concept or GTFO - v17 (30th December, 2017).pdf")
        );
        assert_ne!(sequence_key("report.pdf"), sequence_key("summary.pdf"));
        assert_eq!(sequence_key("12.pdf"), None);
        assert_eq!(sequence_key("IMG_1234.JPG"), None);
        assert_eq!(sequence_key("file042.txt"), None);
    }

    #[test]
    fn natural_order() {
        let mut v = vec!["v10", "v9", "v1.10", "v1.2"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, vec!["v1.2", "v1.10", "v9", "v10"]);
    }
}
