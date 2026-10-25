//! Advisors, semantic operations, and command acceptance.

mod common;

use std::fs;

use common::{dir, env, file};
use filestore::advise::{self, Action};
use filestore::scan::{self, NoHooks, Operation, ScanRequest};
use filestore::semantic::{self, NewRevision, ParentRef};
use filestore::{Error, ledger};
use serde_json::json;

fn suggestion(e: &common::Env, advisor: &str) -> Option<advise::Suggestion> {
    advise::suggestions(&e.cat).unwrap().into_iter().find(|s| s.advisor == advisor)
}

fn blob_at(e: &common::Env, path: &str) -> String {
    e.state(path).unwrap().blob.unwrap()
}

#[test]
fn accepting_a_scan_suggestion_records_provenance_and_rejects_stale_ones() {
    let e = env();
    e.write("a.txt", b"a");
    e.write("b.txt", b"b");
    e.scan(Operation::Inventory, vec![dir("", None)]);
    let s = suggestion(&e, "unidentified-files").expect("unidentified files are suggested for Verify");
    assert_eq!(s.evidence_total, 2);
    let Action::Scan { operation, targets } = &s.action else { panic!() };
    assert_eq!(*operation, Operation::Verify);
    assert_eq!(targets.len(), 1, "a directory whose files all need Verify is one target");

    let r = advise::accept_scan(&e.cat, &s.id).unwrap();
    let advisor: String = e
        .cat
        .conn
        .query_row(
            "SELECT p.advisor FROM scans s JOIN action_provenance p ON p.id = s.provenance_id WHERE s.id = ?1",
            [r.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(advisor, "unidentified-files");
    scan::run(&e.cat.conn, e.cat.root(), r.id, &NoHooks).unwrap();
    assert!(suggestion(&e, "unidentified-files").is_none());

    // The same proposal is no longer offered once the catalog changed.
    assert!(matches!(advise::accept_scan(&e.cat, &s.id), Err(Error::Conflict(_))));
}

#[test]
fn generating_suggestions_writes_nothing() {
    let e = env();
    e.write("a.txt", b"a");
    e.scan(Operation::Inventory, vec![dir("", None)]);
    let changes_before = e.cat.conn.total_changes();
    advise::suggestions(&e.cat).unwrap();
    assert_eq!(e.cat.conn.total_changes(), changes_before);
}

#[test]
fn changed_content_at_a_path_suggests_a_lineage() {
    let e = env();
    e.write("report.txt", b"draft one");
    e.scan(Operation::Verify, vec![dir("", None)]);
    let a = blob_at(&e, "report.txt");
    std::thread::sleep(std::time::Duration::from_millis(10));
    e.write("report.txt", b"draft two, longer");
    e.scan(Operation::Inventory, vec![dir("", None)]);
    assert!(suggestion(&e, "lineage-successor").is_none(), "unverified content cannot be a revision");
    assert!(suggestion(&e, "modified-files").is_some());
    e.scan(Operation::Verify, vec![file("report.txt")]);
    let b = blob_at(&e, "report.txt");
    assert_ne!(a, b);

    let s = suggestion(&e, "lineage-successor").expect("A then B at one path");
    let Action::CreateLineage { revisions, .. } = &s.action else { panic!("{:?}", s.action) };
    assert_eq!(revisions.iter().map(|r| r.blob.clone()).collect::<Vec<_>>(), vec![a.clone(), b.clone()]);

    let prov = advise::record_provenance(&e.cat.conn, &s, None).unwrap();
    let lineage = semantic::create_lineage(
        &e.cat.conn,
        "Report",
        &[
            NewRevision { blob: a.clone(), label: Some("v1".into()), parents: vec![] },
            NewRevision { blob: b.clone(), label: Some("v2".into()), parents: vec![ParentRef::Index(0)] },
        ],
        Some(prov),
    )
    .unwrap();
    assert!(suggestion(&e, "lineage-successor").is_none(), "already recorded");
    let view = semantic::lineage_detail(&e.cat.conn, &lineage).unwrap();
    assert_eq!(view.revisions.len(), 2);
    assert_eq!(view.revisions[1].parents, vec![view.revisions[0].id.clone()]);
    assert_eq!(view.revisions[0].paths, Vec::<String>::new(), "v1 has no current location");
    assert_eq!(view.revisions[1].paths, vec!["report.txt".to_string()]);
    let ops = semantic::history(&e.cat.conn, 10).unwrap();
    assert_eq!(ops[0].advisor.as_deref(), Some("lineage-successor"));
}

#[test]
fn moved_files_are_suggested_for_verify_not_transferred() {
    let e = env();
    e.write("old/x.txt", b"x");
    e.scan(Operation::Verify, vec![dir("", None)]);
    let x = blob_at(&e, "old/x.txt");
    fs::rename(e.root.join("old"), e.root.join("new")).unwrap();
    e.scan(Operation::Inventory, vec![dir("", None)]);
    let s = suggestion(&e, "possible-moves").expect("same inode elsewhere");
    assert!(s.evidence[0].detail.contains("old/x.txt"), "{}", s.evidence[0].detail);
    assert_eq!(e.state("new/x.txt").unwrap().content, "unidentified");
    assert!(suggestion(&e, "unidentified-files").is_none(), "not suggested twice");
    let r = advise::accept_scan(&e.cat, &s.id).unwrap();
    scan::run(&e.cat.conn, e.cat.root(), r.id, &NoHooks).unwrap();
    assert_eq!(blob_at(&e, "new/x.txt"), x, "Verify recovers the Blob");
}

#[test]
fn version_like_names_are_proposed_in_natural_order() {
    let e = env();
    e.write("Manual v10 (2020).pdf", b"ten");
    e.write("Manual v9 (2019).pdf", b"nine");
    e.write("Unrelated.pdf", b"other");
    e.scan(Operation::Verify, vec![dir("", None)]);
    let s = suggestion(&e, "name-sequence").unwrap();
    let Action::CreateLineage { title, revisions } = &s.action else { panic!() };
    assert_eq!(title, "Manual v9");
    let paths: Vec<_> = revisions.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(paths, vec!["Manual v9 (2019).pdf", "Manual v10 (2020).pdf"]);
}

#[test]
fn tag_hierarchies_are_exclusive_per_target() {
    let e = env();
    e.write("a.txt", b"a");
    e.write("b.txt", b"a");
    e.scan(Operation::Verify, vec![dir("", None)]);
    let blob = blob_at(&e, "a.txt");
    assert_eq!(blob, blob_at(&e, "b.txt"), "identical bytes share one Blob");
    let topic = semantic::create_hierarchy(&e.cat.conn, "Topic").unwrap();
    let other = semantic::create_hierarchy(&e.cat.conn, "Other").unwrap();
    let cs = semantic::create_tag(&e.cat.conn, &topic, None, "CS").unwrap();
    let db = semantic::create_tag(&e.cat.conn, &topic, Some(&cs), "Databases").unwrap();
    let x = semantic::create_tag(&e.cat.conn, &other, None, "X").unwrap();
    assert!(semantic::create_tag(&e.cat.conn, &topic, None, "CS").is_err(), "unique sibling names");

    semantic::assign(&e.cat.conn, "blob", &blob, &topic, Some(&cs)).unwrap();
    semantic::assign(&e.cat.conn, "blob", &blob, &topic, Some(&db)).unwrap();
    assert!(semantic::assign(&e.cat.conn, "blob", &blob, &topic, Some(&x)).is_err(), "tag from another hierarchy");
    assert_eq!(e.count("SELECT count(*) FROM tag_assignments"), 1, "one selection per hierarchy");
    // A descendant satisfies the ancestor query.
    let tagged = semantic::tag_targets(&e.cat.conn, &cs).unwrap();
    assert_eq!(tagged.blobs.len(), 1);
    assert_eq!(tagged.blobs[0].paths, vec!["a.txt".to_string(), "b.txt".to_string()]);
    semantic::assign(&e.cat.conn, "blob", &blob, &topic, None).unwrap();
    assert_eq!(e.count("SELECT count(*) FROM tag_assignments"), 0);
    assert_eq!(e.count("SELECT count(*) FROM semantic_ops WHERE kind = 'assign_tag'"), 3);
}

#[test]
fn revision_graphs_reject_cycles_and_foreign_parents() {
    let e = env();
    for (n, d) in [("1", "one"), ("2", "two"), ("3", "three")] {
        e.write(&format!("{n}.txt"), d.as_bytes());
    }
    e.scan(Operation::Verify, vec![dir("", None)]);
    let rev = |p: &str, parents: Vec<ParentRef>| NewRevision { blob: blob_at(&e, p), label: Some(p.into()), parents };
    let l1 = semantic::create_lineage(&e.cat.conn, "L1", &[rev("1.txt", vec![]), rev("2.txt", vec![ParentRef::Index(0)])], None).unwrap();
    let l2 = semantic::create_lineage(&e.cat.conn, "L2", &[rev("3.txt", vec![])], None).unwrap();
    let v1 = semantic::lineage_detail(&e.cat.conn, &l1).unwrap();
    let (r1, r2) = (v1.revisions[0].id.clone(), v1.revisions[1].id.clone());
    let r3 = semantic::lineage_detail(&e.cat.conn, &l2).unwrap().revisions[0].id.clone();

    let cycle = semantic::update_revision(&e.cat.conn, &r1, None, Some(vec![r2.clone()]));
    assert!(matches!(cycle, Err(Error::Invalid(_))), "{cycle:?}");
    let foreign = semantic::add_revision(&e.cat.conn, &l1, &rev("3.txt", vec![ParentRef::Id(r3)]), None);
    assert!(foreign.is_err(), "parents stay within one Lineage");
    // The same content may appear again in a Lineage.
    semantic::add_revision(&e.cat.conn, &l1, &rev("1.txt", vec![ParentRef::Id(r2.clone())]), None).unwrap();
    semantic::tombstone_revision(&e.cat.conn, &r2).unwrap();
    assert!(semantic::add_revision(&e.cat.conn, &l1, &rev("2.txt", vec![ParentRef::Id(r2)]), None).is_err());
    let blob_changed = e.cat.conn.execute("UPDATE revisions SET blob_id = blob_id + 1 WHERE public_id = ?1", [&r1]);
    assert!(blob_changed.is_err(), "revision content identity is immutable");
}

#[test]
fn ledger_replays_a_retried_command_once() {
    let e = env();
    e.write("a.txt", b"a");
    let req = ScanRequest { operation: Operation::Inventory, targets: vec![dir("", None)] };
    let payload = json!({"operation": "inventory", "targets": req.targets});
    let submit = |p: &serde_json::Value| ledger::run(&e.cat, Some("req-1"), "submit_scan", p, |c| scan::submit(&c.conn, &req, None));
    let (first, fresh) = submit(&payload).unwrap();
    let (again, fresh2) = submit(&payload).unwrap();
    assert!(fresh && !fresh2);
    assert_eq!(first, again);
    assert_eq!(e.count("SELECT count(*) FROM scans"), 1);
    let other = json!({"operation": "verify"});
    assert!(matches!(submit(&other), Err(Error::Conflict(_))), "same ID, different payload");
    // A failed first attempt consumes nothing.
    let failed = ledger::run(&e.cat, Some("req-2"), "submit_scan", &payload, |c| {
        scan::submit(&c.conn, &ScanRequest { operation: Operation::Verify, targets: vec![] }, None)
    });
    assert!(failed.is_err());
    assert_eq!(e.count("SELECT count(*) FROM command_ledger"), 1);
}
