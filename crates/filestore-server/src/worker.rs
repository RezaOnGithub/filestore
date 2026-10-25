//! The single worker thread. It owns the catalog connection and executes
//! every database job and scan in FIFO order, so a running scan has
//! exclusive access: queries submitted meanwhile wait behind it.
//!
//! Only `Shared` is touched from other threads: scan progress, cancellation
//! flags, and a snapshot of catalog info. None of it reads the database.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

use filestore::query::{self, CatalogInfo};
use filestore::scan::{self, Hooks, Progress, RunOutcome};
use filestore::{Catalog, Error, Result};
use serde::Serialize;

pub type Job = Box<dyn FnOnce(&mut Worker) + Send>;

#[derive(Serialize, Clone, Default)]
pub struct Status {
    /// Progress of the running scan, if any.
    pub busy: Option<Progress>,
    pub queued: Vec<i64>,
    /// Increments whenever catalog data changes; clients refetch on change.
    pub data_version: u64,
    pub info: Option<CatalogInfo>,
    pub last_scan: Option<RunOutcome>,
    pub recovered: usize,
}

#[derive(Default)]
pub struct Shared {
    pub status: Mutex<Status>,
    pub cancel_running: AtomicBool,
    pub cancel_queued: Mutex<HashSet<i64>>,
}

pub struct Worker {
    pub cat: Catalog,
    shared: Arc<Shared>,
    jobs: Sender<Job>,
}

#[derive(Clone)]
pub struct Handle {
    jobs: Sender<Job>,
    pub shared: Arc<Shared>,
}

pub fn spawn(cat: Catalog) -> Result<Handle> {
    let (tx, rx) = channel::<Job>();
    let shared = Arc::new(Shared::default());
    shared.status.lock().unwrap().recovered = cat.recovered;
    let mut worker = Worker {
        cat,
        shared: shared.clone(),
        jobs: tx.clone(),
    };
    worker.changed();
    for id in scan::queued_scans(&worker.cat.conn)? {
        worker.enqueue_scan(id);
    }
    std::thread::Builder::new()
        .name("filestore-worker".into())
        .spawn(move || {
            for job in rx {
                // A panicking job must not take the only worker down with it.
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(&mut worker)));
                if r.is_err() {
                    worker.recover_after_panic();
                }
            }
        })
        .map_err(|e| Error::io("starting the worker thread", e))?;
    Ok(Handle { jobs: tx, shared })
}

impl Handle {
    /// Run `f` on the worker and await its result.
    pub async fn call<R, F>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&mut Worker) -> Result<R> + Send + 'static,
        R: Send + 'static,
    {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.jobs
            .send(Box::new(move |w: &mut Worker| {
                let _ = tx.send(f(w));
            }))
            .map_err(|_| Error::Conflict("the worker has stopped".into()))?;
        rx.await
            .map_err(|_| Error::Conflict("the worker dropped the request".into()))?
    }

    pub fn status(&self) -> Status {
        self.shared.status.lock().unwrap().clone()
    }

    /// Request cancellation of a running or queued scan. Does not touch the
    /// database, so it takes effect while a scan holds the worker.
    pub fn cancel(&self, scan_id: i64) -> &'static str {
        let st = self.shared.status.lock().unwrap();
        if st.busy.as_ref().is_some_and(|p| p.scan_id == scan_id) {
            self.shared.cancel_running.store(true, Ordering::SeqCst);
            "running"
        } else if st.queued.contains(&scan_id) {
            self.shared.cancel_queued.lock().unwrap().insert(scan_id);
            "queued"
        } else {
            "not_found"
        }
    }
}

struct WorkerHooks<'a>(&'a Shared);

impl Hooks for WorkerHooks<'_> {
    fn progress(&self, p: &Progress) {
        self.0.status.lock().unwrap().busy = Some(p.clone());
    }

    fn cancelled(&self) -> bool {
        self.0.cancel_running.load(Ordering::SeqCst)
    }
}

impl Worker {
    /// Queue execution of a durable scan request behind current jobs.
    pub fn enqueue_scan(&mut self, id: i64) {
        self.shared.status.lock().unwrap().queued.push(id);
        let _ = self.jobs.send(Box::new(move |w: &mut Worker| w.run_scan(id)));
    }

    /// Record that catalog data changed.
    pub fn changed(&mut self) {
        let info = query::info(&self.cat).ok();
        let mut st = self.shared.status.lock().unwrap();
        st.data_version += 1;
        if info.is_some() {
            st.info = info;
        }
    }

    fn recover_after_panic(&mut self) {
        eprintln!("worker: a job panicked; continuing");
        let running = self.shared.status.lock().unwrap().busy.take().map(|p| p.scan_id);
        if let Some(id) = running {
            scan::abort_after_panic(&self.cat.conn, id);
        } else if !self.cat.conn.is_autocommit() {
            let _ = self.cat.conn.execute_batch("ROLLBACK");
        }
        self.shared.cancel_running.store(false, Ordering::SeqCst);
        self.changed();
    }

    fn run_scan(&mut self, id: i64) {
        self.shared.status.lock().unwrap().queued.retain(|q| *q != id);
        if self.shared.cancel_queued.lock().unwrap().remove(&id) {
            if let Err(e) = scan::cancel_queued(&self.cat.conn, id) {
                eprintln!("cancelling scan #{id}: {e}");
            }
            self.changed();
            return;
        }
        self.shared.cancel_running.store(false, Ordering::SeqCst);
        self.shared.status.lock().unwrap().busy = Some(Progress {
            scan_id: id,
            ..Default::default()
        });
        let root = self.cat.root().to_path_buf();
        let result = scan::run(&self.cat.conn, &root, id, &WorkerHooks(&self.shared));
        match &result {
            Ok(r) => eprintln!(
                "scan #{id}: {}{}",
                r.outcome,
                r.error.as_deref().map(|e| format!(" ({e})")).unwrap_or_default()
            ),
            Err(e) => eprintln!("scan #{id}: {e}"),
        }
        {
            let mut st = self.shared.status.lock().unwrap();
            st.busy = None;
            st.last_scan = result.ok();
        }
        self.shared.cancel_running.store(false, Ordering::SeqCst);
        self.changed();
    }
}
