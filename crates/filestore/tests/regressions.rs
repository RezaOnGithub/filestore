//! Regressions found in review: each test reproduces a scenario where the
//! evidence state was wrong or misleading before the fix.

mod common;

use std::cell::Cell;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::{Duration, UNIX_EPOCH};

use common::{Env, big, dir, env, file};
use filestore::project;
use filestore::scan::{self, Hooks, Operation};
use rusqlite::params;

const BIG: usize = 3 * 1024 * 1024 + 123;

/// Publish a synthetic Verify scan whose only read opened `path` (same
/// object, unchanged metadata) and then failed mid-read, exactly as the
/// scanner records an EIO: content 'unreadable', location confirmed.
fn publish_mid_read_eio(e: &Env, path: &str) {
    let conn = &e.cat.conn;
    let now = filestore::ids::now_ms();
    let epoch: i64 = conn.query_row("SELECT max(id) FROM epochs", [], |r| r.get(0)).unwrap();
    let loc: i64 = conn
        .query_row("SELECT id FROM locations WHERE path = ?1", [path.as_bytes()], |r| r.get(0))
        .unwrap();
    let (dev, ino, size, mt, ct, mode, nlink): (i64, i64, i64, i64, i64, i64, i64) = conn
        .query_row(
            "SELECT dev, ino, size, mtime_ns, ctime_ns, mode, nlink FROM observations
             WHERE location_id = ?1 AND role = 'opened' ORDER BY id DESC LIMIT 1",
            [loc],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
        )
        .unwrap();
    conn.execute(
        "INSERT INTO scans (public_id, operation, targets, requested_at, state, epoch_id, started_at)
         VALUES ('scan_synth_eio', 'verify', '[]', ?1, 'running', ?2, ?1)",
        params![now, epoch],
    )
    .unwrap();
    let scan = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO observations (scan_id, location_id, role, kind, dev, ino, size, mtime_ns, ctime_ns,
                                   mode, nlink, observed_at, event_order)
         VALUES (?1, ?2, 'opened', 'file', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 2)",
        params![scan, loc, dev, ino, size, mt, ct, mode, nlink, now],
    )
    .unwrap();
    let obs = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO reads (scan_id, location_id, opened_obs_id, attempt, method, started_at, start_order)
         VALUES (?1, ?2, ?3, 1, 'full', ?4, 1)",
        params![scan, loc, obs, now],
    )
    .unwrap();
    let read = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO read_results (read_id, content, reason, bytes_read, location_check, location_checked_at,
                                   finished_at, end_order)
         VALUES (?1, 'unreadable', 'read failed after 2097152 bytes: EIO', 2097152, 'confirmed', ?2, ?2, 3)",
        params![read, now],
    )
    .unwrap();
    let seq: i64 = conn
        .query_row("SELECT coalesce(max(publication_seq), 0) + 1 FROM scans", [], |r| r.get(0))
        .unwrap();
    conn.execute(
        "UPDATE scans SET state = 'closed', outcome = 'finished_with_gaps', finished_at = ?2, publication_seq = ?3
         WHERE id = ?1",
        params![scan, now, seq],
    )
    .unwrap();
    project::apply_scan(conn, scan, seq).unwrap();
}

#[test]
fn a_sample_match_does_not_hide_a_newer_mid_read_failure() {
    let e = env();
    e.write("f.bin", &big(1, BIG));
    e.scan(Operation::Verify, vec![dir("", None)]);
    publish_mid_read_eio(&e, "f.bin");
    assert_eq!(e.state("f.bin").unwrap().issue.as_deref(), Some("unreadable"));

    let r = e.scan(Operation::Check, vec![file("f.bin")]);
    assert_eq!(r.summary["counts"]["sample_match"], 1);
    let s = e.state("f.bin").unwrap();
    assert_eq!(s.issue.as_deref(), Some("unreadable"), "sampled blocks can miss the failing region: {s:?}");

    e.scan(Operation::Verify, vec![file("f.bin")]);
    assert_eq!(e.state("f.bin").unwrap().issue, None, "a successful full read resolves it");
}

#[test]
fn a_successful_open_resolves_an_earlier_open_failure() {
    let e = env();
    e.write("d/f.bin", &big(2, BIG));
    e.scan(Operation::Verify, vec![dir("", None)]);
    let d = e.root.join("d");
    fs::set_permissions(&d, fs::Permissions::from_mode(0o600)).unwrap();
    let r = e.scan(Operation::Verify, vec![file("d/f.bin")]);
    fs::set_permissions(&d, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(r.outcome, "finished_with_gaps");
    assert_eq!(e.state("d/f.bin").unwrap().issue.as_deref(), Some("open_failed"));
    e.scan(Operation::Check, vec![file("d/f.bin")]);
    let s = e.state("d/f.bin").unwrap();
    assert_eq!(s.issue, None, "{s:?}");
    assert_eq!(s.content, "verified");
    assert_eq!(s.check.as_deref(), Some("sample_match"));
}

#[test]
fn far_future_timestamps_do_not_panic() {
    let e = env();
    e.write("future.txt", b"x");
    let f = fs::File::options().write(true).open(e.root.join("future.txt")).unwrap();
    f.set_modified(UNIX_EPOCH + Duration::from_secs(10_500_000_000)).unwrap(); // ~2302
    drop(f);
    let r = e.scan(Operation::Verify, vec![dir("", None)]);
    assert_eq!(r.outcome, "succeeded");
    assert_eq!(e.state("future.txt").unwrap().content, "verified");
}

struct SwapRoot {
    root: PathBuf,
    done: Cell<bool>,
}

impl Hooks for SwapRoot {
    fn cancelled(&self) -> bool {
        if !self.done.get() {
            self.done.set(true);
            fs::rename(&self.root, self.root.with_file_name("files.old")).unwrap();
            fs::create_dir(&self.root).unwrap();
        }
        false
    }
}

#[test]
fn replacing_the_root_during_a_scan_publishes_nothing() {
    let e = env();
    e.write("a.txt", b"a");
    e.write("b.txt", b"b");
    let r = e.scan_with(Operation::Verify, vec![dir("", None)], &SwapRoot { root: e.root.clone(), done: Cell::new(false) });
    assert_eq!(r.outcome, "failed");
    assert_eq!(r.publication_seq, None);
    assert!(e.state("a.txt").is_none(), "nothing became current");
    assert_eq!(e.count("SELECT count(*) FROM blobs"), 2, "recognized content is still admitted");
    let next = e.scan(Operation::Inventory, vec![dir("", None)]);
    assert_eq!(next.outcome, "failed", "the replacement needs adoption");
}

#[test]
fn a_file_target_is_not_swallowed_by_a_same_path_directory_target() {
    let e = env();
    e.write("a", b"content");
    e.scan(Operation::Verify, vec![dir("a", None), file("a")]);
    assert_eq!(e.count("SELECT count(*) FROM reads"), 1, "the requested file was read");
}

#[test]
fn a_replaced_object_is_compared_with_the_paths_last_verified_content() {
    let e = env();
    let data = big(5, BIG);
    e.write("f.bin", &data);
    e.scan(Operation::Verify, vec![dir("", None)]);
    let a = e.state("f.bin").unwrap().blob;
    let mut edited = data.clone();
    edited[0] ^= 0xff;
    std::thread::sleep(Duration::from_millis(10));
    fs::write(e.root.join("f.bin"), &edited).unwrap();
    e.scan(Operation::Check, vec![dir("", None)]);
    assert_eq!(e.state("f.bin").unwrap().content, "changed");
    // Restore the original bytes as a new object (new inode).
    fs::write(e.root.join("tmp"), &data).unwrap();
    fs::rename(e.root.join("tmp"), e.root.join("f.bin")).unwrap();
    e.scan(Operation::Check, vec![dir("", None)]);
    let s = e.state("f.bin").unwrap();
    assert_eq!(s.content, "plausible", "compared with {a:?}: {s:?}");
    assert_eq!(s.blob, a);
    assert!(!s.verified, "a sample match never verifies");
}

#[test]
fn rebuild_after_adoption_matches() {
    let e = env();
    e.write("x/a.bin", &big(7, BIG));
    e.write("b.txt", b"b");
    e.scan(Operation::Verify, vec![dir("", None)]);
    let old = e.root.with_file_name("files.prev");
    fs::rename(&e.root, &old).unwrap();
    fs::create_dir(&e.root).unwrap();
    fs::create_dir(e.root.join("x")).unwrap();
    fs::copy(old.join("x/a.bin"), e.root.join("x/a.bin")).unwrap();
    let r = e.scan(Operation::Inventory, vec![dir("", None)]);
    assert_eq!(r.outcome, "failed");
    scan::adopt_root_identity(&e.cat.conn, e.cat.root()).unwrap();
    e.scan(Operation::Inventory, vec![dir("", None)]);
    e.scan(Operation::Check, vec![dir("", None)]);
    assert_eq!(e.state("x/a.bin").unwrap().content, "plausible");
    e.assert_rebuild_matches();
}

#[test]
fn evidence_tables_reject_deletes_and_location_rewrites() {
    let e = env();
    e.write("a.txt", b"a");
    e.write("big.bin", &big(9, BIG));
    e.scan(Operation::Verify, vec![dir("", None)]);
    e.scan(Operation::Verify, vec![file("missing.txt")]);
    for table in ["read_results", "enumeration_results", "baselines", "derived_metadata", "diagnostics"] {
        assert!(e.count(&format!("SELECT count(*) FROM {table}")) > 0, "{table} has rows to protect");
    }
    for sql in [
        "DELETE FROM read_results",
        "DELETE FROM enumeration_results",
        "DELETE FROM baselines",
        "DELETE FROM derived_metadata",
        "DELETE FROM diagnostics",
        "UPDATE locations SET path = X'7a' WHERE path = X'612e747874'",
        "UPDATE epochs SET root_ino = 1",
    ] {
        assert!(e.cat.conn.execute(sql, []).is_err(), "{sql} must be rejected");
    }
}
