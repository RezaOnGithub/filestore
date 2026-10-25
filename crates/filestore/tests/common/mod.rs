//! Shared fixtures for integration tests: a catalog and scan root under
//! the repository's `.local/test-tmp`.
#![allow(dead_code)]

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use filestore::Catalog;
use filestore::project;
use filestore::scan::{self, Hooks, NoHooks, Operation, RunOutcome, ScanRequest, Target, TargetKind};
use rusqlite::OptionalExtension;

pub struct Env {
    pub _tmp: tempfile::TempDir,
    pub root: PathBuf,
    pub cat: Catalog,
}

pub fn env() -> Env {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.local/test-tmp");
    fs::create_dir_all(&base).unwrap();
    let tmp = tempfile::Builder::new().prefix("it").tempdir_in(&base).unwrap();
    let root = tmp.path().join("files");
    fs::create_dir_all(&root).unwrap();
    let catdir = tmp.path().join("catalog");
    Catalog::init(&catdir, &root).unwrap();
    let cat = Catalog::open(&catdir).unwrap();
    Env { _tmp: tmp, root, cat }
}

pub fn dir(path: &str, depth: Option<u32>) -> Target {
    Target { path: path.into(), kind: TargetKind::Dir, depth }
}

pub fn file(path: &str) -> Target {
    Target { path: path.into(), kind: TargetKind::File, depth: None }
}

impl Env {
    pub fn scan_with(&self, operation: Operation, targets: Vec<Target>, hooks: &dyn Hooks) -> RunOutcome {
        let r = scan::submit(&self.cat.conn, &ScanRequest { operation, targets }, None).unwrap();
        scan::run(&self.cat.conn, self.cat.root(), r.id, hooks).unwrap()
    }

    pub fn scan(&self, operation: Operation, targets: Vec<Target>) -> RunOutcome {
        self.scan_with(operation, targets, &NoHooks)
    }

    pub fn write(&self, rel: &str, data: &[u8]) {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, data).unwrap();
    }

    pub fn ino(&self, rel: &str) -> i64 {
        fs::symlink_metadata(self.root.join(rel)).unwrap().ino() as i64
    }

    /// (content, blob public id, verified at?, issue, check_result) of the
    /// current occurrence at `path`.
    pub fn state(&self, path: &str) -> Option<Occ> {
        self.cat
            .conn
            .query_row(
                "SELECT o.kind, o.content, b.public_id, o.verified_at IS NOT NULL, o.issue, o.check_result,
                        o.ino, o.listing_enum_id, o.listing_stale, o.size, o.ev_size, o.mtime_ns, o.ev_mtime_ns
                 FROM occurrences o JOIN locations l ON l.id = o.location_id
                 LEFT JOIN blobs b ON b.id = o.content_blob_id
                 WHERE l.path = ?1 AND o.ended_seq IS NULL
                   AND o.epoch_id = (SELECT max(id) FROM epochs)",
                [path.as_bytes()],
                |r| {
                    Ok(Occ {
                        kind: r.get(0)?,
                        content: r.get(1)?,
                        blob: r.get(2)?,
                        verified: r.get(3)?,
                        issue: r.get(4)?,
                        check: r.get(5)?,
                        ino: r.get(6)?,
                        listing: r.get(7)?,
                        listing_stale: r.get(8)?,
                        modified: r.get::<_, Option<i64>>(9)? != r.get::<_, Option<i64>>(10)?
                            || r.get::<_, Option<i64>>(11)? != r.get::<_, Option<i64>>(12)?,
                    })
                },
            )
            .optional()
            .unwrap()
    }

    pub fn ended(&self, path: &str) -> Vec<String> {
        let mut stmt = self
            .cat
            .conn
            .prepare(
                "SELECT o.end_reason FROM occurrences o JOIN locations l ON l.id = o.location_id
                 WHERE l.path = ?1 AND o.ended_seq IS NOT NULL ORDER BY o.id",
            )
            .unwrap();
        stmt.query_map([path.as_bytes()], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect()
    }

    pub fn count(&self, sql: &str) -> i64 {
        self.cat.conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    pub fn assert_rebuild_matches(&self) {
        let before = project::snapshot(&self.cat.conn).unwrap();
        project::rebuild(&self.cat.conn).unwrap();
        let after = project::snapshot(&self.cat.conn).unwrap();
        assert_eq!(before, after, "rebuilt projection differs from the incremental one");
    }
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct Occ {
    pub kind: Option<String>,
    pub content: String,
    pub blob: Option<String>,
    pub verified: bool,
    pub issue: Option<String>,
    pub check: Option<String>,
    pub ino: Option<i64>,
    pub listing: Option<i64>,
    pub listing_stale: i64,
    pub modified: bool,
}

pub fn big(seed: u8, len: usize) -> Vec<u8> {
    let mut x: u32 = 0x9e37_79b9 ^ u32::from(seed);
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect()
}

