//! Catalog directory, database connection, process lock, and recovery.
//!
//! Layout: `<catalog>/catalog.sqlite` and `<catalog>/lock`. The scan root is
//! stored relative to the catalog directory and must lie outside it.

use std::cell::RefCell;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use rusqlite::{Connection, OptionalExtension, params};
use rustix::fs::{FlockOperation, flock};

use crate::ids::{new_public_id, now_ms};
use crate::{Error, Result, pathenc, query};

pub const SCHEMA_VERSION: i64 = 1;
const SCHEMA: &str = include_str!("schema.sql");
const DB_FILE: &str = "catalog.sqlite";
pub const DEFAULT_VERIFY_HORIZON_DAYS: i64 = 730;

pub struct Catalog {
    pub conn: Connection,
    dir: PathBuf,
    root: PathBuf,
    _lock: File,
    /// Scans recovered as abandoned when this process opened the catalog.
    pub recovered: usize,
    /// Subtree counts, keyed by projection state (see `query::aggregates`).
    pub(crate) aggregates: RefCell<Option<Arc<query::Aggregates>>>,
}

impl Catalog {
    /// Create a new catalog in `dir` for the scan root `root`.
    pub fn init(dir: &Path, root: &Path) -> Result<()> {
        fs::create_dir_all(dir).map_err(|e| Error::io(format!("creating {}", dir.display()), e))?;
        let dir = dir
            .canonicalize()
            .map_err(|e| Error::io(format!("resolving {}", dir.display()), e))?;
        let root = root
            .canonicalize()
            .map_err(|e| Error::io(format!("resolving root {}", root.display()), e))?;
        if !root.is_dir() {
            return Err(Error::Invalid(format!("root {} is not a directory", root.display())));
        }
        if dir.starts_with(&root) {
            return Err(Error::Invalid(format!(
                "catalog {} must not be inside the scan root {}",
                dir.display(),
                root.display()
            )));
        }
        if root.starts_with(&dir) {
            return Err(Error::Invalid(format!(
                "scan root {} must not be inside the catalog {}",
                root.display(),
                dir.display()
            )));
        }
        let db_path = dir.join(DB_FILE);
        if db_path.exists() {
            return Err(Error::Conflict(format!("{} is already initialized", dir.display())));
        }
        let _lock = lock(&dir)?;
        let conn = Connection::open(&db_path)?;
        configure(&conn)?;
        let rel = relative_to(&dir, &root);
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(SCHEMA)?;
        let now = now_ms();
        for (k, v) in [
            ("schema_version", SCHEMA_VERSION.to_string()),
            ("created_at", now.to_string()),
            ("archive_id", new_public_id("archive")),
            ("root_path", pathenc::encode(rel.as_os_str().as_bytes())),
            ("verify_horizon_days", DEFAULT_VERIFY_HORIZON_DAYS.to_string()),
        ] {
            tx.execute("INSERT INTO meta (key, value) VALUES (?1, ?2)", params![k, v])?;
        }
        tx.execute(
            "INSERT INTO locations (id, parent_id, name, path) VALUES (1, NULL, X'', X'')",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn exists(dir: &Path) -> bool {
        dir.join(DB_FILE).exists()
    }

    /// Open an existing catalog, taking the process lock and recovering
    /// sessions left running by a previous process.
    pub fn open(dir: &Path) -> Result<Catalog> {
        let dir = dir
            .canonicalize()
            .map_err(|e| Error::io(format!("resolving {}", dir.display()), e))?;
        let db_path = dir.join(DB_FILE);
        if !db_path.exists() {
            return Err(Error::NotFound(format!(
                "no catalog in {} (initialize it with a root first)",
                dir.display()
            )));
        }
        let lock = lock(&dir)?;
        let conn = Connection::open(&db_path)?;
        configure(&conn)?;
        let version: i64 = get_meta(&conn, "schema_version")?
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| Error::Invalid("catalog has no schema version".into()))?;
        if version != SCHEMA_VERSION {
            return Err(Error::Invalid(format!(
                "catalog schema version {version} is not supported (expected {SCHEMA_VERSION})"
            )));
        }
        let rel = get_meta(&conn, "root_path")?
            .ok_or_else(|| Error::Invalid("catalog has no root".into()))?;
        let rel = pathenc::decode(&rel)?;
        let root = normalize(&dir.join(OsStr::from_bytes(&rel)));
        let recovered = conn.execute(
            "UPDATE scans SET state = 'closed', outcome = 'abandoned', finished_at = ?1,
                    error = 'interrupted: the process exited before finalization'
             WHERE state = 'running'",
            [now_ms()],
        )?;
        Ok(Catalog {
            conn,
            dir,
            root,
            _lock: lock,
            recovered,
            aggregates: RefCell::new(None),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        get_meta(&self.conn, key)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        set_meta(&self.conn, key, value)
    }

    pub fn verify_horizon_days(&self) -> i64 {
        self.meta("verify_horizon_days")
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_VERIFY_HORIZON_DAYS)
    }

    /// Latest publication sequence, 0 before the first published scan.
    pub fn publication_seq(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT coalesce(max(publication_seq), 0) FROM scans", [], |r| r.get(0))?)
    }
}

pub fn get_meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .optional()?)
}

pub fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

pub fn delete_meta(conn: &Connection, key: &str) -> Result<()> {
    conn.execute("DELETE FROM meta WHERE key = ?1", [key])?;
    Ok(())
}

fn configure(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA busy_timeout = 5000;",
    )?;
    Ok(())
}

/// Exclusive process-lifetime lock on the catalog. The OS releases it when
/// the process exits.
fn lock(dir: &Path) -> Result<File> {
    let path = dir.join("lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| Error::io(format!("opening {}", path.display()), e))?;
    flock(&file, FlockOperation::NonBlockingLockExclusive).map_err(|_| {
        Error::Conflict(format!(
            "catalog {} is in use by another Filestore process",
            dir.display()
        ))
    })?;
    Ok(file)
}

fn relative_to(base: &Path, target: &Path) -> PathBuf {
    let b: Vec<Component> = base.components().collect();
    let t: Vec<Component> = target.components().collect();
    let common = b.iter().zip(&t).take_while(|(x, y)| x == y).count();
    let mut rel = PathBuf::new();
    for _ in common..b.len() {
        rel.push("..");
    }
    for c in &t[common..] {
        rel.push(c);
    }
    if rel.as_os_str().is_empty() {
        rel.push(".");
    }
    rel
}

/// Lexically resolve `..` components (the stored root is relative to the
/// canonical catalog directory, so this matches what `init` saw).
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// A nestable atomic section implemented with SQLite savepoints. Outside a
/// transaction it behaves like `BEGIN DEFERRED`; inside one it nests, so
/// callers (such as an idempotency ledger) can wrap operations that are
/// themselves atomic. Dropping without `commit` rolls the section back.
pub struct Atomic<'c> {
    conn: &'c Connection,
    name: String,
    done: bool,
}

pub fn atomic(conn: &Connection) -> Result<Atomic<'_>> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let name = format!("fs_sp{}", NEXT.fetch_add(1, Ordering::Relaxed));
    conn.execute_batch(&format!("SAVEPOINT {name}"))?;
    Ok(Atomic {
        conn,
        name,
        done: false,
    })
}

impl Atomic<'_> {
    pub fn commit(mut self) -> Result<()> {
        self.done = true;
        self.conn.execute_batch(&format!("RELEASE {}", self.name))?;
        Ok(())
    }
}

impl Drop for Atomic<'_> {
    fn drop(&mut self) {
        if !self.done {
            let _ = self
                .conn
                .execute_batch(&format!("ROLLBACK TO {0}; RELEASE {0}", self.name));
        }
    }
}

impl std::ops::Deref for Atomic<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.conn
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths() {
        assert_eq!(
            relative_to(Path::new("/a/b/catalog"), Path::new("/a/b/files")),
            PathBuf::from("../files")
        );
        assert_eq!(
            normalize(&Path::new("/a/b/catalog").join("../files")),
            PathBuf::from("/a/b/files")
        );
        assert_eq!(
            relative_to(Path::new("/x/cat"), Path::new("/y/z")),
            PathBuf::from("../../y/z")
        );
    }
}
