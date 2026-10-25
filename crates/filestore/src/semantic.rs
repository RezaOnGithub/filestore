//! Semantic records: exclusive tag hierarchies, Lineages with revision DAGs,
//! and the append-only operation log. Only user operations change these, and
//! none of them rewrites scanner evidence.

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::ids::{new_public_id, now_ms};
use crate::{Error, Result, pathenc};

pub const ACTOR: &str = "local-user";

fn record_op(conn: &Connection, kind: &str, summary: &str, changes: Value, provenance: Option<i64>) -> Result<String> {
    let id = new_public_id("op");
    conn.execute(
        "INSERT INTO semantic_ops (public_id, kind, actor, summary, changes, provenance_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![id, kind, ACTOR, summary, changes.to_string(), provenance, now_ms()],
    )?;
    Ok(id)
}

fn check_name(name: &str, what: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::Invalid(format!("{what} must not be empty")));
    }
    if name.contains('/') {
        return Err(Error::Invalid(format!("{what} must not contain '/'")));
    }
    Ok(name.to_string())
}

fn lookup(conn: &Connection, table: &str, public_id: &str) -> Result<i64> {
    conn.query_row(&format!("SELECT id FROM {table} WHERE public_id = ?1"), [public_id], |r| r.get(0))
        .optional()?
        .ok_or_else(|| Error::NotFound(public_id.to_string()))
}

pub fn blob_id(conn: &Connection, public_id: &str) -> Result<i64> {
    lookup(conn, "blobs", public_id)
}

// ----- tags ---------------------------------------------------------------

#[derive(Serialize, Clone)]
pub struct TagView {
    pub id: String,
    pub name: String,
    pub parent: Option<String>,
    pub path: String,
    pub usage: i64,
}

#[derive(Serialize)]
pub struct HierarchyView {
    pub id: String,
    pub name: String,
    pub tags: Vec<TagView>,
}

#[derive(Serialize, Clone, Debug)]
pub struct AssignmentView {
    pub hierarchy: String,
    pub hierarchy_name: String,
    pub tag: String,
    pub tag_path: String,
}

struct TagRow {
    public_id: String,
    hierarchy_id: i64,
    parent_id: Option<i64>,
    name: String,
}

/// All tags keyed by row ID, with their hierarchy-relative paths.
struct TagIndex {
    tags: HashMap<i64, TagRow>,
    hierarchies: HashMap<i64, (String, String)>,
}

impl TagIndex {
    fn load(conn: &Connection) -> Result<TagIndex> {
        let mut tags = HashMap::new();
        let mut stmt = conn.prepare("SELECT id, public_id, hierarchy_id, parent_id, name FROM tags")?;
        for row in stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                TagRow {
                    public_id: r.get(1)?,
                    hierarchy_id: r.get(2)?,
                    parent_id: r.get(3)?,
                    name: r.get(4)?,
                },
            ))
        })? {
            let (id, t) = row?;
            tags.insert(id, t);
        }
        let mut hierarchies = HashMap::new();
        let mut stmt = conn.prepare("SELECT id, public_id, name FROM tag_hierarchies")?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, (r.get(1)?, r.get(2)?))))? {
            let (id, h) = row?;
            hierarchies.insert(id, h);
        }
        Ok(TagIndex { tags, hierarchies })
    }

    fn path(&self, mut id: i64) -> String {
        let mut parts = Vec::new();
        while let Some(t) = self.tags.get(&id) {
            parts.push(t.name.as_str());
            match t.parent_id {
                Some(p) => id = p,
                None => break,
            }
        }
        parts.reverse();
        parts.join("/")
    }

    fn assignment(&self, tag_id: i64) -> Option<AssignmentView> {
        let t = self.tags.get(&tag_id)?;
        let (hid, hname) = self.hierarchies.get(&t.hierarchy_id)?;
        Some(AssignmentView {
            hierarchy: hid.clone(),
            hierarchy_name: hname.clone(),
            tag: t.public_id.clone(),
            tag_path: self.path(tag_id),
        })
    }

    /// The tag and all of its descendants.
    fn subtree(&self, root: i64) -> HashSet<i64> {
        let mut out = HashSet::from([root]);
        loop {
            let before = out.len();
            for (id, t) in &self.tags {
                if t.parent_id.is_some_and(|p| out.contains(&p)) {
                    out.insert(*id);
                }
            }
            if out.len() == before {
                return out;
            }
        }
    }
}

pub fn list_tags(conn: &Connection) -> Result<Vec<HierarchyView>> {
    let idx = TagIndex::load(conn)?;
    let mut usage: HashMap<i64, i64> = HashMap::new();
    let mut stmt = conn.prepare("SELECT tag_id, count(*) FROM tag_assignments GROUP BY tag_id")?;
    for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
        let (t, n) = row?;
        usage.insert(t, n);
    }
    let mut out: Vec<HierarchyView> = idx
        .hierarchies
        .iter()
        .map(|(hid, (public, name))| {
            let mut tags: Vec<TagView> = idx
                .tags
                .iter()
                .filter(|(_, t)| t.hierarchy_id == *hid)
                .map(|(id, t)| TagView {
                    id: t.public_id.clone(),
                    name: t.name.clone(),
                    parent: t.parent_id.and_then(|p| idx.tags.get(&p)).map(|p| p.public_id.clone()),
                    path: idx.path(*id),
                    usage: usage.get(id).copied().unwrap_or(0),
                })
                .collect();
            tags.sort_by(|a, b| a.path.cmp(&b.path));
            HierarchyView {
                id: public.clone(),
                name: name.clone(),
                tags,
            }
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

pub fn create_hierarchy(conn: &Connection, name: &str) -> Result<String> {
    let name = check_name(name, "hierarchy name")?;
    let tx = crate::catalog::atomic(conn)?;
    let exists: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM tag_hierarchies WHERE name = ?1)",
        [&name],
        |r| r.get(0),
    )?;
    if exists {
        return Err(Error::Conflict(format!("a hierarchy named {name:?} already exists")));
    }
    let id = new_public_id("hier");
    tx.execute(
        "INSERT INTO tag_hierarchies (public_id, name, created_at) VALUES (?1, ?2, ?3)",
        params![id, name, now_ms()],
    )?;
    record_op(
        &tx,
        "create_hierarchy",
        &format!("Created tag hierarchy {name}"),
        json!([{"entity": id, "before": null, "after": {"name": name}}]),
        None,
    )?;
    tx.commit()?;
    Ok(id)
}

pub fn create_tag(conn: &Connection, hierarchy: &str, parent: Option<&str>, name: &str) -> Result<String> {
    let name = check_name(name, "tag name")?;
    let tx = crate::catalog::atomic(conn)?;
    let hid = lookup(&tx, "tag_hierarchies", hierarchy)?;
    let parent_id = match parent {
        Some(p) => {
            let (pid, phid): (i64, i64) = tx
                .query_row("SELECT id, hierarchy_id FROM tags WHERE public_id = ?1", [p], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .optional()?
                .ok_or_else(|| Error::NotFound(p.to_string()))?;
            if phid != hid {
                return Err(Error::Invalid("the parent tag belongs to another hierarchy".into()));
            }
            Some(pid)
        }
        None => None,
    };
    let taken: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM tags WHERE hierarchy_id = ?1 AND parent_id IS ?2 AND name = ?3)",
        params![hid, parent_id, name],
        |r| r.get(0),
    )?;
    if taken {
        return Err(Error::Conflict(format!("a sibling tag named {name:?} already exists")));
    }
    let id = new_public_id("tag");
    tx.execute(
        "INSERT INTO tags (public_id, hierarchy_id, parent_id, name, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, hid, parent_id, name, now_ms()],
    )?;
    let path = TagIndex::load(&tx)?.path(tx.last_insert_rowid());
    record_op(
        &tx,
        "create_tag",
        &format!("Created tag {path}"),
        json!([{"entity": id, "before": null, "after": {"hierarchy": hierarchy, "parent": parent, "name": name}}]),
        None,
    )?;
    tx.commit()?;
    Ok(id)
}

pub fn rename_tag(conn: &Connection, tag: &str, name: &str) -> Result<()> {
    let name = check_name(name, "tag name")?;
    let tx = crate::catalog::atomic(conn)?;
    let (id, hid, parent, old): (i64, i64, Option<i64>, String) = tx
        .query_row(
            "SELECT id, hierarchy_id, parent_id, name FROM tags WHERE public_id = ?1",
            [tag],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?
        .ok_or_else(|| Error::NotFound(tag.to_string()))?;
    if old == name {
        return Ok(());
    }
    let taken: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM tags WHERE hierarchy_id = ?1 AND parent_id IS ?2 AND name = ?3)",
        params![hid, parent, name],
        |r| r.get(0),
    )?;
    if taken {
        return Err(Error::Conflict(format!("a sibling tag named {name:?} already exists")));
    }
    tx.execute("UPDATE tags SET name = ?2 WHERE id = ?1", params![id, name])?;
    record_op(
        &tx,
        "rename_tag",
        &format!("Renamed tag {old} to {name}"),
        json!([{"entity": tag, "before": {"name": old}, "after": {"name": name}}]),
        None,
    )?;
    tx.commit()?;
    Ok(())
}

fn target_row(conn: &Connection, kind: &str, public_id: &str) -> Result<(i64, String)> {
    let (table, label_sql) = match kind {
        "blob" => ("blobs", "public_id"),
        "lineage" => ("lineages", "title"),
        "revision" => ("revisions", "coalesce(label, public_id)"),
        other => return Err(Error::Invalid(format!("unknown tag target kind {other:?}"))),
    };
    conn.query_row(
        &format!("SELECT id, {label_sql} FROM {table} WHERE public_id = ?1"),
        [public_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(public_id.to_string()))
}

/// Select `tag` (or nothing) from `hierarchy` for a target. Each target holds
/// at most one tag per hierarchy.
pub fn assign(conn: &Connection, kind: &str, target: &str, hierarchy: &str, tag: Option<&str>) -> Result<()> {
    let tx = crate::catalog::atomic(conn)?;
    let (target_id, label) = target_row(&tx, kind, target)?;
    let hid = lookup(&tx, "tag_hierarchies", hierarchy)?;
    let idx = TagIndex::load(&tx)?;
    let before: Option<i64> = tx
        .query_row(
            "SELECT tag_id FROM tag_assignments WHERE target_kind = ?1 AND target_id = ?2 AND hierarchy_id = ?3",
            params![kind, target_id, hid],
            |r| r.get(0),
        )
        .optional()?;
    let after = match tag {
        Some(t) => {
            let (tid, thid): (i64, i64) = tx
                .query_row("SELECT id, hierarchy_id FROM tags WHERE public_id = ?1", [t], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .optional()?
                .ok_or_else(|| Error::NotFound(t.to_string()))?;
            if thid != hid {
                return Err(Error::Invalid("the tag belongs to another hierarchy".into()));
            }
            Some(tid)
        }
        None => None,
    };
    if before == after {
        return Ok(());
    }
    match after {
        Some(tid) => {
            tx.execute(
                "INSERT INTO tag_assignments (target_kind, target_id, hierarchy_id, tag_id, assigned_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (target_kind, target_id, hierarchy_id)
                 DO UPDATE SET tag_id = excluded.tag_id, assigned_at = excluded.assigned_at",
                params![kind, target_id, hid, tid, now_ms()],
            )?;
        }
        None => {
            tx.execute(
                "DELETE FROM tag_assignments WHERE target_kind = ?1 AND target_id = ?2 AND hierarchy_id = ?3",
                params![kind, target_id, hid],
            )?;
        }
    }
    let path = |t: Option<i64>| t.map(|t| idx.path(t));
    let summary = match (path(before), path(after)) {
        (None, Some(a)) => format!("Tagged {kind} {label} as {a}"),
        (Some(b), None) => format!("Removed tag {b} from {kind} {label}"),
        (Some(b), Some(a)) => format!("Retagged {kind} {label} from {b} to {a}"),
        (None, None) => unreachable!(),
    };
    record_op(
        &tx,
        "assign_tag",
        &summary,
        json!([{
            "entity": {"target_kind": kind, "target": target, "hierarchy": hierarchy},
            "before": before.and_then(|t| idx.tags.get(&t)).map(|t| t.public_id.clone()),
            "after": tag,
        }]),
        None,
    )?;
    tx.commit()?;
    Ok(())
}

/// Assignments for many targets of one kind, keyed by row ID.
pub fn assignments_for(conn: &Connection, kind: &str, ids: &[i64]) -> Result<HashMap<i64, Vec<AssignmentView>>> {
    let mut out: HashMap<i64, Vec<AssignmentView>> = HashMap::new();
    if ids.is_empty() {
        return Ok(out);
    }
    let any: bool = conn.query_row("SELECT EXISTS (SELECT 1 FROM tag_assignments)", [], |r| r.get(0))?;
    if !any {
        return Ok(out);
    }
    let idx = TagIndex::load(conn)?;
    let mut stmt = conn.prepare(
        "SELECT target_id, tag_id FROM tag_assignments
         WHERE target_kind = ?1 AND target_id IN (SELECT value FROM json_each(?2))",
    )?;
    let rows = stmt.query_map(params![kind, serde_json::to_string(ids)?], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (target, tag) = row?;
        if let Some(a) = idx.assignment(tag) {
            out.entry(target).or_default().push(a);
        }
    }
    for v in out.values_mut() {
        v.sort_by(|a, b| a.hierarchy_name.cmp(&b.hierarchy_name));
    }
    Ok(out)
}

#[derive(Serialize)]
pub struct TaggedBlob {
    pub id: String,
    pub size: i64,
    pub tag_path: String,
    pub paths: Vec<String>,
}

#[derive(Serialize)]
pub struct TaggedOther {
    pub id: String,
    pub label: String,
    pub tag_path: String,
    pub lineage: Option<String>,
}

#[derive(Serialize)]
pub struct TagTargets {
    pub tag: TagView,
    pub blobs: Vec<TaggedBlob>,
    pub lineages: Vec<TaggedOther>,
    pub revisions: Vec<TaggedOther>,
}

/// Everything tagged with `tag` or one of its descendants.
pub fn tag_targets(conn: &Connection, tag: &str) -> Result<TagTargets> {
    let idx = TagIndex::load(conn)?;
    let tid = lookup(conn, "tags", tag)?;
    let t = &idx.tags[&tid];
    let set: Vec<i64> = idx.subtree(tid).into_iter().collect();
    let set_json = serde_json::to_string(&set)?;
    let mut blobs = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT b.id, b.public_id, b.size, ta.tag_id FROM tag_assignments ta JOIN blobs b ON b.id = ta.target_id
         WHERE ta.target_kind = 'blob' AND ta.tag_id IN (SELECT value FROM json_each(?1))
         ORDER BY b.id",
    )?;
    let rows: Vec<(i64, String, i64, i64)> = stmt
        .query_map([&set_json], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (bid, public, size, tag_id) in rows {
        blobs.push(TaggedBlob {
            id: public,
            size,
            tag_path: idx.path(tag_id),
            paths: current_paths(conn, bid)?,
        });
    }
    let mut lineages = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT l.public_id, l.title, ta.tag_id FROM tag_assignments ta JOIN lineages l ON l.id = ta.target_id
         WHERE ta.target_kind = 'lineage' AND ta.tag_id IN (SELECT value FROM json_each(?1))",
    )?;
    for row in stmt.query_map([&set_json], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?)))? {
        let (id, label, tag_id) = row?;
        lineages.push(TaggedOther {
            id,
            label,
            tag_path: idx.path(tag_id),
            lineage: None,
        });
    }
    let mut revisions = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT r.public_id, coalesce(r.label, r.public_id), ta.tag_id, l.public_id
         FROM tag_assignments ta JOIN revisions r ON r.id = ta.target_id JOIN lineages l ON l.id = r.lineage_id
         WHERE ta.target_kind = 'revision' AND ta.tag_id IN (SELECT value FROM json_each(?1))",
    )?;
    for row in stmt.query_map([&set_json], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, String>(3)?))
    })? {
        let (id, label, tag_id, lineage) = row?;
        revisions.push(TaggedOther {
            id,
            label,
            tag_path: idx.path(tag_id),
            lineage: Some(lineage),
        });
    }
    Ok(TagTargets {
        tag: TagView {
            id: t.public_id.clone(),
            name: t.name.clone(),
            parent: t.parent_id.and_then(|p| idx.tags.get(&p)).map(|p| p.public_id.clone()),
            path: idx.path(tid),
            usage: (blobs.len() + lineages.len() + revisions.len()) as i64,
        },
        blobs,
        lineages,
        revisions,
    })
}

/// Current locations where a Blob is verified or plausibly present.
pub fn current_paths(conn: &Connection, blob: i64) -> Result<Vec<String>> {
    let mut stmt = conn.prepare_cached(
        "SELECT l.path FROM occurrences o JOIN locations l ON l.id = o.location_id
         WHERE o.content_blob_id = ?1 AND o.content IN ('verified', 'plausible') AND o.ended_seq IS NULL
           AND o.epoch_id = (SELECT max(id) FROM epochs)
         ORDER BY l.path",
    )?;
    let rows = stmt.query_map([blob], |r| r.get::<_, Vec<u8>>(0))?;
    let mut out = Vec::new();
    for p in rows {
        out.push(pathenc::encode(&p?));
    }
    Ok(out)
}

// ----- lineages ----------------------------------------------------------

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(untagged)]
pub enum ParentRef {
    /// Index of an earlier revision in the same request.
    Index(usize),
    /// Public ID of an existing revision.
    Id(String),
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct NewRevision {
    pub blob: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub parents: Vec<ParentRef>,
}

fn clean_label(label: &Option<String>) -> Option<String> {
    label.as_ref().map(|l| l.trim().to_string()).filter(|l| !l.is_empty())
}

fn live_revision(conn: &Connection, public_id: &str, lineage: i64) -> Result<i64> {
    let (id, lid, tomb): (i64, i64, Option<i64>) = conn
        .query_row(
            "SELECT id, lineage_id, tombstoned_at FROM revisions WHERE public_id = ?1",
            [public_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
        .ok_or_else(|| Error::NotFound(public_id.to_string()))?;
    if lid != lineage {
        return Err(Error::Invalid("revision parents must belong to the same Lineage".into()));
    }
    if tomb.is_some() {
        return Err(Error::Invalid(format!("revision {public_id} has been removed")));
    }
    Ok(id)
}

fn insert_revision(
    conn: &Connection,
    lineage: i64,
    rev: &NewRevision,
    earlier: &[(i64, String)],
) -> Result<(i64, String, Value)> {
    let blob = blob_id(conn, &rev.blob)?;
    let label = clean_label(&rev.label);
    let mut parents = Vec::new();
    for p in &rev.parents {
        let pid = match p {
            ParentRef::Index(i) => earlier
                .get(*i)
                .map(|e| e.0)
                .ok_or_else(|| Error::Invalid(format!("parent index {i} does not name an earlier revision")))?,
            ParentRef::Id(id) => live_revision(conn, id, lineage)?,
        };
        if !parents.contains(&pid) {
            parents.push(pid);
        }
    }
    let public = new_public_id("rev");
    conn.execute(
        "INSERT INTO revisions (public_id, lineage_id, blob_id, label, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![public, lineage, blob, label, now_ms()],
    )?;
    let id = conn.last_insert_rowid();
    let mut parent_public = Vec::new();
    for pid in parents {
        conn.execute(
            "INSERT INTO revision_parents (revision_id, parent_id) VALUES (?1, ?2)",
            params![id, pid],
        )?;
        parent_public.push(conn.query_row("SELECT public_id FROM revisions WHERE id = ?1", [pid], |r| {
            r.get::<_, String>(0)
        })?);
    }
    let change = json!({"entity": public, "before": null, "after": {"blob": rev.blob, "label": label, "parents": parent_public}});
    Ok((id, public, change))
}

pub fn create_lineage(conn: &Connection, title: &str, revisions: &[NewRevision], provenance: Option<i64>) -> Result<String> {
    let title = title.trim();
    if title.is_empty() {
        return Err(Error::Invalid("a Lineage needs a title".into()));
    }
    let tx = crate::catalog::atomic(conn)?;
    let public = new_public_id("lin");
    tx.execute(
        "INSERT INTO lineages (public_id, title, created_at) VALUES (?1, ?2, ?3)",
        params![public, title, now_ms()],
    )?;
    let lid = tx.last_insert_rowid();
    let mut changes = vec![json!({"entity": public, "before": null, "after": {"title": title}})];
    let mut created: Vec<(i64, String)> = Vec::new();
    for rev in revisions {
        let (id, rp, change) = insert_revision(&tx, lid, rev, &created)?;
        created.push((id, rp));
        changes.push(change);
    }
    record_op(
        &tx,
        "create_lineage",
        &format!("Created Lineage {title:?} with {} revision(s)", revisions.len()),
        Value::Array(changes),
        provenance,
    )?;
    tx.commit()?;
    Ok(public)
}

fn lineage_row(conn: &Connection, public_id: &str) -> Result<(i64, String, Option<i64>)> {
    conn.query_row(
        "SELECT id, title, tombstoned_at FROM lineages WHERE public_id = ?1",
        [public_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(public_id.to_string()))
}

pub fn add_revision(conn: &Connection, lineage: &str, rev: &NewRevision, provenance: Option<i64>) -> Result<String> {
    let tx = crate::catalog::atomic(conn)?;
    let (lid, title, tomb) = lineage_row(&tx, lineage)?;
    if tomb.is_some() {
        return Err(Error::Invalid("the Lineage has been removed".into()));
    }
    if rev.parents.iter().any(|p| matches!(p, ParentRef::Index(_))) {
        return Err(Error::Invalid("parents of an added revision are revision IDs".into()));
    }
    let (_, public, change) = insert_revision(&tx, lid, rev, &[])?;
    record_op(
        &tx,
        "add_revision",
        &format!(
            "Added revision {} to {title:?}",
            clean_label(&rev.label).unwrap_or_else(|| public.clone())
        ),
        json!([change]),
        provenance,
    )?;
    tx.commit()?;
    Ok(public)
}

pub fn update_revision(
    conn: &Connection,
    revision: &str,
    label: Option<Option<String>>,
    parents: Option<Vec<String>>,
) -> Result<()> {
    let tx = crate::catalog::atomic(conn)?;
    let (id, lid, old_label, tomb): (i64, i64, Option<String>, Option<i64>) = tx
        .query_row(
            "SELECT id, lineage_id, label, tombstoned_at FROM revisions WHERE public_id = ?1",
            [revision],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?
        .ok_or_else(|| Error::NotFound(revision.to_string()))?;
    if tomb.is_some() {
        return Err(Error::Invalid("the revision has been removed".into()));
    }
    let mut before = serde_json::Map::new();
    let mut after = serde_json::Map::new();
    if let Some(label) = label {
        let label = clean_label(&label);
        if label != old_label {
            tx.execute("UPDATE revisions SET label = ?2 WHERE id = ?1", params![id, label])?;
            before.insert("label".into(), json!(old_label));
            after.insert("label".into(), json!(label));
        }
    }
    if let Some(parents) = parents {
        let old: Vec<String> = parent_ids(&tx, id)?;
        let mut new_ids = Vec::new();
        for p in &parents {
            let pid = live_revision(&tx, p, lid)?;
            if pid == id {
                return Err(Error::Invalid("a revision cannot be its own parent".into()));
            }
            if !new_ids.contains(&pid) {
                new_ids.push(pid);
            }
        }
        let descendants = descendants(&tx, id)?;
        if new_ids.iter().any(|p| descendants.contains(p)) {
            return Err(Error::Invalid("that parent would create a cycle".into()));
        }
        tx.execute("DELETE FROM revision_parents WHERE revision_id = ?1", [id])?;
        for pid in &new_ids {
            tx.execute(
                "INSERT INTO revision_parents (revision_id, parent_id) VALUES (?1, ?2)",
                params![id, pid],
            )?;
        }
        let new: Vec<String> = parent_ids(&tx, id)?;
        if old != new {
            before.insert("parents".into(), json!(old));
            after.insert("parents".into(), json!(new));
        }
    }
    if after.is_empty() {
        return Ok(());
    }
    record_op(
        &tx,
        "update_revision",
        &format!("Edited revision {}", old_label.unwrap_or_else(|| revision.to_string())),
        json!([{"entity": revision, "before": before, "after": after}]),
        None,
    )?;
    tx.commit()?;
    Ok(())
}

fn parent_ids(conn: &Connection, rev: i64) -> Result<Vec<String>> {
    let mut stmt = conn.prepare_cached(
        "SELECT p.public_id FROM revision_parents rp JOIN revisions p ON p.id = rp.parent_id
         WHERE rp.revision_id = ?1 ORDER BY p.id",
    )?;
    let rows = stmt.query_map([rev], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn descendants(conn: &Connection, rev: i64) -> Result<HashSet<i64>> {
    let mut stmt = conn.prepare(
        "WITH RECURSIVE d(id) AS (
             SELECT revision_id FROM revision_parents WHERE parent_id = ?1
             UNION SELECT rp.revision_id FROM revision_parents rp JOIN d ON rp.parent_id = d.id)
         SELECT id FROM d",
    )?;
    let rows = stmt.query_map([rev], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn tombstone_revision(conn: &Connection, revision: &str) -> Result<()> {
    let tx = crate::catalog::atomic(conn)?;
    let n = tx.execute(
        "UPDATE revisions SET tombstoned_at = ?2 WHERE public_id = ?1 AND tombstoned_at IS NULL",
        params![revision, now_ms()],
    )?;
    if n == 0 {
        return Err(Error::NotFound(revision.to_string()));
    }
    record_op(
        &tx,
        "remove_revision",
        &format!("Removed revision {revision}"),
        json!([{"entity": revision, "before": {"removed": false}, "after": {"removed": true}}]),
        None,
    )?;
    tx.commit()?;
    Ok(())
}

pub fn rename_lineage(conn: &Connection, lineage: &str, title: &str) -> Result<()> {
    let title = title.trim();
    if title.is_empty() {
        return Err(Error::Invalid("a Lineage needs a title".into()));
    }
    let tx = crate::catalog::atomic(conn)?;
    let (id, old, _) = lineage_row(&tx, lineage)?;
    if old == title {
        return Ok(());
    }
    tx.execute("UPDATE lineages SET title = ?2 WHERE id = ?1", params![id, title])?;
    record_op(
        &tx,
        "rename_lineage",
        &format!("Renamed Lineage {old:?} to {title:?}"),
        json!([{"entity": lineage, "before": {"title": old}, "after": {"title": title}}]),
        None,
    )?;
    tx.commit()?;
    Ok(())
}

pub fn tombstone_lineage(conn: &Connection, lineage: &str) -> Result<()> {
    let tx = crate::catalog::atomic(conn)?;
    let (id, title, tomb) = lineage_row(&tx, lineage)?;
    if tomb.is_some() {
        return Ok(());
    }
    tx.execute("UPDATE lineages SET tombstoned_at = ?2 WHERE id = ?1", params![id, now_ms()])?;
    record_op(
        &tx,
        "remove_lineage",
        &format!("Removed Lineage {title:?}"),
        json!([{"entity": lineage, "before": {"removed": false}, "after": {"removed": true}}]),
        None,
    )?;
    tx.commit()?;
    Ok(())
}

#[derive(Serialize)]
pub struct LineageSummary {
    pub id: String,
    pub title: String,
    pub created_at: i64,
    pub revisions: i64,
    pub tags: Vec<AssignmentView>,
}

pub fn list_lineages(conn: &Connection) -> Result<Vec<LineageSummary>> {
    let mut stmt = conn.prepare(
        "SELECT l.id, l.public_id, l.title, l.created_at,
                (SELECT count(*) FROM revisions r WHERE r.lineage_id = l.id AND r.tombstoned_at IS NULL)
         FROM lineages l WHERE l.tombstoned_at IS NULL ORDER BY l.title COLLATE NOCASE",
    )?;
    let rows: Vec<(i64, LineageSummary)> = stmt
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                LineageSummary {
                    id: r.get(1)?,
                    title: r.get(2)?,
                    created_at: r.get(3)?,
                    revisions: r.get(4)?,
                    tags: Vec::new(),
                },
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let ids: Vec<i64> = rows.iter().map(|r| r.0).collect();
    let mut tags = assignments_for(conn, "lineage", &ids)?;
    Ok(rows
        .into_iter()
        .map(|(id, mut s)| {
            s.tags = tags.remove(&id).unwrap_or_default();
            s
        })
        .collect())
}

#[derive(Serialize)]
pub struct RevisionView {
    pub id: String,
    pub label: Option<String>,
    pub blob: String,
    pub blob_size: i64,
    pub format: Option<String>,
    pub created_at: i64,
    pub removed: bool,
    pub parents: Vec<String>,
    pub tags: Vec<AssignmentView>,
    pub paths: Vec<String>,
}

#[derive(Serialize)]
pub struct LineageView {
    pub id: String,
    pub title: String,
    pub created_at: i64,
    pub removed: bool,
    pub tags: Vec<AssignmentView>,
    pub revisions: Vec<RevisionView>,
}

/// A Lineage with its revisions in topological order (parents first).
pub fn lineage_detail(conn: &Connection, lineage: &str) -> Result<LineageView> {
    let (lid, title, tomb) = lineage_row(conn, lineage)?;
    let created_at: i64 = conn.query_row("SELECT created_at FROM lineages WHERE id = ?1", [lid], |r| r.get(0))?;
    let mut stmt = conn.prepare(
        "SELECT r.id, r.public_id, r.label, b.id, b.public_id, b.size, r.created_at, r.tombstoned_at,
                (SELECT json_extract(payload, '$.format') FROM derived_metadata d
                 WHERE d.blob_id = b.id AND d.extractor = 'sniff' ORDER BY d.id DESC LIMIT 1)
         FROM revisions r JOIN blobs b ON b.id = r.blob_id WHERE r.lineage_id = ?1 ORDER BY r.id",
    )?;
    #[allow(clippy::type_complexity)]
    let rows: Vec<(i64, String, Option<String>, i64, String, i64, i64, Option<i64>, Option<String>)> = stmt
        .query_map([lid], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let ids: Vec<i64> = rows.iter().map(|r| r.0).collect();
    let mut tags = assignments_for(conn, "revision", &ids)?;
    let mut parents: HashMap<i64, Vec<i64>> = HashMap::new();
    let mut stmt = conn.prepare(
        "SELECT rp.revision_id, rp.parent_id FROM revision_parents rp JOIN revisions r ON r.id = rp.revision_id
         WHERE r.lineage_id = ?1",
    )?;
    for row in stmt.query_map([lid], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
        let (c, p) = row?;
        parents.entry(c).or_default().push(p);
    }
    let public: HashMap<i64, String> = rows.iter().map(|r| (r.0, r.1.clone())).collect();
    // Kahn's algorithm, ties broken by creation order.
    let mut placed: HashSet<i64> = HashSet::new();
    let mut order: Vec<usize> = Vec::new();
    while order.len() < rows.len() {
        let next = rows.iter().position(|r| {
            !placed.contains(&r.0) && parents.get(&r.0).is_none_or(|ps| ps.iter().all(|p| placed.contains(p)))
        });
        let Some(i) = next else {
            // Unreachable for a validated DAG; fall back to creation order.
            order.extend((0..rows.len()).filter(|i| !placed.contains(&rows[*i].0)));
            break;
        };
        placed.insert(rows[i].0);
        order.push(i);
    }
    let mut revisions = Vec::new();
    for i in order {
        let r = &rows[i];
        let mut ps: Vec<String> = parents
            .get(&r.0)
            .map(|v| v.iter().filter_map(|p| public.get(p).cloned()).collect())
            .unwrap_or_default();
        ps.sort();
        revisions.push(RevisionView {
            id: r.1.clone(),
            label: r.2.clone(),
            blob: r.4.clone(),
            blob_size: r.5,
            format: r.8.clone(),
            created_at: r.6,
            removed: r.7.is_some(),
            parents: ps,
            tags: tags.remove(&r.0).unwrap_or_default(),
            paths: current_paths(conn, r.3)?,
        });
    }
    Ok(LineageView {
        id: lineage.to_string(),
        title,
        created_at,
        removed: tomb.is_some(),
        tags: assignments_for(conn, "lineage", &[lid])?.remove(&lid).unwrap_or_default(),
        revisions,
    })
}

#[derive(Serialize)]
pub struct RevisionRef {
    pub revision: String,
    pub label: Option<String>,
    pub lineage: String,
    pub lineage_title: String,
}

/// Live revisions referring to a Blob.
pub fn revisions_of_blob(conn: &Connection, blob: i64) -> Result<Vec<RevisionRef>> {
    let mut stmt = conn.prepare_cached(
        "SELECT r.public_id, r.label, l.public_id, l.title FROM revisions r JOIN lineages l ON l.id = r.lineage_id
         WHERE r.blob_id = ?1 AND r.tombstoned_at IS NULL AND l.tombstoned_at IS NULL ORDER BY r.id",
    )?;
    let rows = stmt.query_map([blob], |r| {
        Ok(RevisionRef {
            revision: r.get(0)?,
            label: r.get(1)?,
            lineage: r.get(2)?,
            lineage_title: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[derive(Serialize)]
pub struct OpView {
    pub id: String,
    pub kind: String,
    pub actor: String,
    pub summary: String,
    pub changes: Value,
    pub created_at: i64,
    pub advisor: Option<String>,
}

pub fn history(conn: &Connection, limit: i64) -> Result<Vec<OpView>> {
    let mut stmt = conn.prepare(
        "SELECT o.public_id, o.kind, o.actor, o.summary, o.changes, o.created_at, p.advisor
         FROM semantic_ops o LEFT JOIN action_provenance p ON p.id = o.provenance_id
         ORDER BY o.id DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit], |r| {
        Ok(OpView {
            id: r.get(0)?,
            kind: r.get(1)?,
            actor: r.get(2)?,
            summary: r.get(3)?,
            changes: serde_json::from_str(&r.get::<_, String>(4)?).unwrap_or(Value::Null),
            created_at: r.get(5)?,
            advisor: r.get(6)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Record a root-identity adoption in the operation log.
pub fn record_adoption(conn: &Connection, epoch_public: &str, detail: Value) -> Result<()> {
    record_op(
        conn,
        "adopt_root_identity",
        "Adopted the root's new filesystem identity as a new identity epoch",
        json!([{"entity": epoch_public, "before": null, "after": detail}]),
        None,
    )?;
    Ok(())
}
