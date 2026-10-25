//! Scanner and projection integration tests against a real filesystem tree.

mod common;

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};

use common::{big, dir, env, file};
use filestore::Catalog;
use filestore::scan::{self, Hooks, Operation, TargetKind};

const BIG: usize = 3 * 1024 * 1024 + 123;

#[test]
fn inventory_reads_no_content_and_verify_recognizes_blobs() {
    let e = env();
    e.write("a.txt", b"hello");
    e.write("sub/b.bin", &big(1, BIG));
    e.write("sub/c.txt", b"hello");

    let r = e.scan(Operation::Inventory, vec![dir("", None)]);
    assert_eq!(r.outcome, "succeeded");
    assert_eq!(r.summary["counts"]["bytes_read"], 0);
    assert_eq!(e.count("SELECT count(*) FROM blobs"), 0);
    assert_eq!(e.count("SELECT count(*) FROM reads"), 0);
    let a = e.state("a.txt").unwrap();
    assert_eq!(a.content, "unidentified");
    assert!(e.state("sub").unwrap().listing.is_some());

    let r = e.scan(Operation::Verify, vec![dir("", None)]);
    assert_eq!(r.outcome, "succeeded", "{:#}", r.summary);
    assert_eq!(e.count("SELECT count(*) FROM blobs"), 2, "a.txt and sub/c.txt share one Blob");
    let a = e.state("a.txt").unwrap();
    let c = e.state("sub/c.txt").unwrap();
    assert_eq!(a.content, "verified");
    assert_eq!(a.blob, c.blob);
    assert_eq!(e.state("sub/b.bin").unwrap().content, "verified");
    // Only the large file needs a baseline; small ones are fully read by Check.
    assert_eq!(e.count("SELECT count(*) FROM baselines"), 1);
    // Every read has a fresh opened observation distinct from its trigger.
    assert_eq!(
        e.count("SELECT count(*) FROM reads WHERE opened_obs_id IS NULL OR opened_obs_id = trigger_obs_id"),
        0
    );
    // A second inventory keeps the occurrence and its verification.
    e.scan(Operation::Inventory, vec![dir("", None)]);
    let a2 = e.state("a.txt").unwrap();
    assert_eq!(a2.content, "verified");
    assert!(a2.verified);
    e.assert_rebuild_matches();
}

#[test]
fn in_place_edit_is_flagged_then_ruled_out_then_reverified() {
    let e = env();
    let data = big(2, BIG);
    e.write("doc.bin", &data);
    e.scan(Operation::Verify, vec![dir("", None)]);
    let v1 = e.state("doc.bin").unwrap();
    assert_eq!(v1.content, "verified");

    // Same inode, same size, first byte changed.
    let mut edited = data.clone();
    edited[0] ^= 0xff;
    let p = e.root.join("doc.bin");
    std::thread::sleep(std::time::Duration::from_millis(10));
    fs::write(&p, &edited).unwrap();
    assert_eq!(e.ino("doc.bin"), v1.ino.unwrap());

    e.scan(Operation::Inventory, vec![dir("", None)]);
    let s = e.state("doc.bin").unwrap();
    assert_eq!(s.content, "verified", "metadata alone does not prove a content change");
    assert!(s.modified, "but the metadata differs from the verifying read");

    let r = e.scan(Operation::Check, vec![dir("", None)]);
    assert_eq!(r.summary["counts"]["sample_mismatch"], 1);
    let s = e.state("doc.bin").unwrap();
    assert_eq!(s.content, "changed");
    assert_eq!(s.blob, v1.blob, "the ruled-out Blob is recorded");

    // Check does not compare against a ruled-out Blob again.
    let r = e.scan(Operation::Check, vec![file("doc.bin")]);
    assert_eq!(r.summary["counts"]["no_baseline"], 1);
    assert_eq!(e.state("doc.bin").unwrap().content, "changed");

    e.scan(Operation::Verify, vec![file("doc.bin")]);
    let s = e.state("doc.bin").unwrap();
    assert_eq!(s.content, "verified");
    assert_ne!(s.blob, v1.blob);
    assert_eq!(e.count("SELECT count(*) FROM blobs"), 2, "the old Blob survives");
    e.assert_rebuild_matches();
}

#[test]
fn check_matches_samples_and_misses_unsampled_edits() {
    let e = env();
    let data = big(3, BIG);
    e.write("f.bin", &data);
    e.scan(Operation::Verify, vec![dir("", None)]);
    let r = e.scan(Operation::Check, vec![dir("", None)]);
    assert_eq!(r.summary["counts"]["sample_match"], 1);
    assert!(r.summary["counts"]["bytes_read"].as_u64().unwrap() <= 16 * 65536);
    let s = e.state("f.bin").unwrap();
    assert_eq!(s.content, "verified", "a match against an unmodified verified file keeps it verified");
    assert_eq!(s.check.as_deref(), Some("sample_match"));

    // Edit a byte between the first two sampled blocks (block 1 of 49).
    let mut edited = data.clone();
    edited[65536 + 10] ^= 0x55;
    std::thread::sleep(std::time::Duration::from_millis(10));
    fs::write(e.root.join("f.bin"), &edited).unwrap();
    e.scan(Operation::Check, vec![dir("", None)]);
    let s = e.state("f.bin").unwrap();
    assert_eq!(s.content, "plausible", "the fixed-sample blind spot: plausible, not verified");
}

#[test]
fn small_files_are_verified_by_check_and_large_unknown_files_need_verify() {
    let e = env();
    e.write("small.txt", b"tiny");
    e.write("large.bin", &big(4, BIG));
    let r = e.scan(Operation::Check, vec![dir("", None)]);
    assert_eq!(r.outcome, "succeeded", "no_baseline is not a gap: identifying content was not requested");
    assert_eq!(e.state("small.txt").unwrap().content, "verified");
    let l = e.state("large.bin").unwrap();
    assert_eq!(l.content, "unidentified");
    assert_eq!(l.check.as_deref(), Some("no_baseline"));
    assert_eq!(r.summary["counts"]["bytes_read"], 4, "no purposeless samples of the large file");
}

#[test]
fn rename_ends_old_occurrence_and_starts_unidentified_one() {
    let e = env();
    e.write("old.txt", b"content");
    e.scan(Operation::Verify, vec![dir("", None)]);
    let old = e.state("old.txt").unwrap();
    fs::rename(e.root.join("old.txt"), e.root.join("new.txt")).unwrap();

    e.scan(Operation::Inventory, vec![dir("", None)]);
    assert!(e.state("old.txt").is_none());
    assert_eq!(e.ended("old.txt"), vec!["absent_in_listing"]);
    let new = e.state("new.txt").unwrap();
    assert_eq!(new.content, "unidentified", "no verification transfers through an inode match");
    assert_eq!(new.ino, old.ino);

    e.scan(Operation::Verify, vec![file("new.txt")]);
    assert_eq!(e.state("new.txt").unwrap().blob, old.blob, "Verify recovers the same Blob ID");
    e.assert_rebuild_matches();
}

#[test]
fn replaced_directory_does_not_acquire_old_descendants() {
    let e = env();
    e.write("d/x.txt", b"x");
    e.write("d/y.txt", b"y");
    e.scan(Operation::Verify, vec![dir("", None)]);
    fs::remove_dir_all(e.root.join("d")).unwrap();
    // Keep the old inode alive elsewhere so the new directory gets a new one.
    fs::create_dir(e.root.join("keep")).unwrap();
    fs::create_dir(e.root.join("d")).unwrap();
    e.write("d/x.txt", b"x");

    // List only the root: the new d is seen, its contents are not.
    e.scan(Operation::Inventory, vec![dir("", Some(0))]);
    assert_eq!(e.ended("d"), vec!["replaced"]);
    assert_eq!(e.ended("d/x.txt"), vec!["ancestor_ended"]);
    assert!(e.state("d/x.txt").is_none(), "old children are not shown under the replacement");
    assert!(e.state("d").unwrap().listing.is_none(), "the replacement's entries are unknown");

    e.scan(Operation::Inventory, vec![dir("d", Some(0))]);
    assert_eq!(e.state("d/x.txt").unwrap().content, "unidentified");
    assert!(e.state("d/y.txt").is_none());
    e.assert_rebuild_matches();
}

#[test]
fn selected_file_verify_does_not_refresh_parent_listing() {
    let e = env();
    e.write("p/a.txt", b"a");
    e.write("p/b.txt", b"b");
    e.scan(Operation::Inventory, vec![dir("", None)]);
    let listing = e.state("p").unwrap().listing;
    let r = e.scan(Operation::Verify, vec![file("p/a.txt")]);
    assert_eq!(r.outcome, "succeeded");
    assert_eq!(e.state("p").unwrap().listing, listing);
    assert_eq!(e.state("p/a.txt").unwrap().content, "verified");
    assert_eq!(e.state("p/b.txt").unwrap().content, "unidentified");
}

#[test]
fn directory_change_makes_listing_stale() {
    let e = env();
    e.write("p/a.txt", b"a");
    e.scan(Operation::Inventory, vec![dir("", None)]);
    assert_eq!(e.state("p").unwrap().listing_stale, 0);
    std::thread::sleep(std::time::Duration::from_millis(10));
    e.write("p/new.txt", b"n");
    e.scan(Operation::Inventory, vec![dir("", Some(0))]);
    assert_eq!(e.state("p").unwrap().listing_stale, 1, "p changed since its listing");
    assert!(e.state("p/new.txt").is_none(), "unlisted names stay unknown");
    e.scan(Operation::Inventory, vec![dir("p", Some(0))]);
    assert_eq!(e.state("p").unwrap().listing_stale, 0);
    assert!(e.state("p/new.txt").is_some());
}

#[test]
fn unreadable_file_is_a_gap_and_keeps_last_known_evidence() {
    let e = env();
    e.write("secret.txt", b"s");
    e.write("ok.txt", b"o");
    e.scan(Operation::Verify, vec![dir("", None)]);
    let p = e.root.join("secret.txt");
    fs::set_permissions(&p, fs::Permissions::from_mode(0o000)).unwrap();
    let r = e.scan(Operation::Verify, vec![dir("", None)]);
    fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(r.outcome, "finished_with_gaps");
    let s = e.state("secret.txt").unwrap();
    assert_eq!(s.content, "verified", "the earlier verification remains last-known");
    assert_eq!(s.issue.as_deref(), Some("open_failed"), "nothing was opened: an access failure");
    assert_eq!(e.state("ok.txt").unwrap().issue, None, "siblings are unaffected");
    e.scan(Operation::Verify, vec![file("secret.txt")]);
    assert_eq!(e.state("secret.txt").unwrap().issue, None, "a later success resolves the issue");
}

#[test]
fn symlinks_are_recorded_not_followed() {
    let e = env();
    e.write("real/data.txt", b"d");
    symlink("real", e.root.join("link")).unwrap();
    symlink("/etc/passwd", e.root.join("escape")).unwrap();
    let r = e.scan(Operation::Verify, vec![dir("", None)]);
    assert_eq!(r.outcome, "succeeded");
    assert_eq!(e.state("link").unwrap().kind.as_deref(), Some("symlink"));
    assert!(e.state("link/data.txt").is_none());
    assert_eq!(e.count("SELECT count(*) FROM blobs"), 1);
    // A file target through a symlink fails resolution instead of escaping.
    let r = e.scan(Operation::Verify, vec![file("link/data.txt")]);
    assert_eq!(r.outcome, "finished_with_gaps");
    let r = e.scan(Operation::Verify, vec![file("escape")]);
    assert_eq!(r.outcome, "finished_with_gaps");
    assert_eq!(e.count("SELECT count(*) FROM blobs"), 1);
}

#[test]
fn closed_scans_reject_new_evidence() {
    let e = env();
    e.write("a.txt", b"a");
    let r = e.scan(Operation::Inventory, vec![dir("", None)]);
    let err = e.cat.conn.execute(
        "INSERT INTO observations (scan_id, location_id, role, observed_at, event_order, kind)
         VALUES (?1, 1, 'anchor', 0, 999, 'dir')",
        [r.scan_id],
    );
    assert!(err.unwrap_err().to_string().contains("scan is not running"));
    let enum_id: i64 = e.cat.conn.query_row("SELECT max(id) FROM enumerations", [], |r| r.get(0)).unwrap();
    // Even a running scan cannot append entries to a closed enumeration.
    e.cat.conn.execute("INSERT INTO scans (public_id, operation, targets, requested_at, state) VALUES ('scan_x', 'inventory', '[]', 0, 'running')", []).unwrap();
    let running: i64 = e.cat.conn.last_insert_rowid();
    let err = e.cat.conn.execute(
        "INSERT INTO observations (scan_id, location_id, role, enumeration_id, observed_at, event_order, kind)
         VALUES (?1, 1, 'entry', ?2, 0, 1, 'file')",
        [running, enum_id],
    );
    assert!(err.unwrap_err().to_string().contains("enumeration is closed"));
    let err = e.cat.conn.execute("UPDATE scans SET outcome = 'failed' WHERE id = ?1", [r.scan_id]);
    assert!(err.is_err());
}

#[test]
fn interrupted_scan_is_abandoned_on_reopen() {
    let e = env();
    e.cat
        .conn
        .execute(
            "INSERT INTO scans (public_id, operation, targets, requested_at, state)
             VALUES ('scan_y', 'verify', '[]', 0, 'running')",
            [],
        )
        .unwrap();
    let dir = e.cat.dir().to_path_buf();
    drop(e.cat);
    let cat = Catalog::open(&dir).unwrap();
    assert_eq!(cat.recovered, 1);
    let (outcome, published): (String, Option<i64>) = cat
        .conn
        .query_row("SELECT outcome, publication_seq FROM scans WHERE public_id = 'scan_y'", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(outcome, "abandoned");
    assert_eq!(published, None);
}

#[test]
fn a_second_process_cannot_open_the_catalog() {
    let e = env();
    assert!(Catalog::open(e.cat.dir()).is_err());
}

struct CancelAfter(std::cell::Cell<u32>);
impl Hooks for CancelAfter {
    fn cancelled(&self) -> bool {
        let n = self.0.get();
        self.0.set(n.saturating_sub(1));
        n == 0
    }
}

#[test]
fn cancellation_finishes_with_gaps_and_publishes_completed_work() {
    let e = env();
    for i in 0..20 {
        e.write(&format!("f{i:02}.txt"), format!("{i}").as_bytes());
    }
    let r = e.scan_with(Operation::Verify, vec![dir("", None)], &CancelAfter(std::cell::Cell::new(6)));
    assert_eq!(r.outcome, "finished_with_gaps");
    assert_eq!(r.summary["cancelled"], true);
    assert!(r.publication_seq.is_some());
    let verified = e.count("SELECT count(*) FROM occurrences WHERE content = 'verified'");
    assert!(verified > 0 && verified < 20, "{verified}");
}

#[test]
fn overlapping_targets_are_normalized() {
    let t = scan::normalize_targets(&[
        dir("a", None),
        file("a/b/c.txt"),
        dir("a/b", Some(2)),
        dir("x", Some(1)),
        file("x/y/z.txt"),
        file("x/y/q/z.txt"),
        dir("x", Some(1)),
    ])
    .unwrap();
    let paths: Vec<_> = t.iter().map(|t| (t.path.as_str(), t.kind)).collect();
    assert_eq!(
        paths,
        vec![("a", TargetKind::Dir), ("x", TargetKind::Dir), ("x/y/q/z.txt", TargetKind::File)]
    );
    assert!(scan::normalize_targets(&[file("")]).is_err());
    assert!(scan::normalize_targets(&[dir("../etc", None)]).is_err());
}
