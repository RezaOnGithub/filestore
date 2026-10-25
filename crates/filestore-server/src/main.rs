//! `filestore`: command-line entry point and HTTP server.

mod api;
mod worker;

use std::net::SocketAddr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use filestore::scan::{self, Hooks, Operation, Progress, ScanRequest, Target, TargetKind};
use filestore::{Catalog, advise, pathenc, project};
use tower_http::services::{ServeDir, ServeFile};

#[derive(Parser)]
#[command(name = "filestore", version, about = "Filestore prototype: file evidence, presence checks, and Lineages")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a catalog for a scan root. The catalog must be outside the root.
    Init {
        #[arg(long)]
        catalog: PathBuf,
        #[arg(long)]
        root: PathBuf,
    },
    /// Serve the web UI and HTTP API (creates the catalog if --root is given).
    Serve {
        #[arg(long)]
        catalog: PathBuf,
        #[arg(long)]
        root: Option<PathBuf>,
        #[arg(long, default_value = "127.0.0.1:7878")]
        listen: SocketAddr,
        /// Directory with the built web UI.
        #[arg(long)]
        web: Option<PathBuf>,
    },
    /// Run one scan in the foreground.
    Scan {
        #[arg(long)]
        catalog: PathBuf,
        /// inventory, verify, or check
        operation: String,
        /// Root-relative paths; the whole root when omitted.
        paths: Vec<PathBuf>,
        /// Descendant-directory levels for directory targets (default: unlimited).
        #[arg(long)]
        depth: Option<u32>,
    },
    /// Rebuild the occurrence projection from published scans.
    Rebuild {
        #[arg(long)]
        catalog: PathBuf,
    },
    /// Print the advisors' current suggestions.
    Suggest {
        #[arg(long)]
        catalog: PathBuf,
    },
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Init { catalog, root } => {
            Catalog::init(&catalog, &root)?;
            println!("initialized {} for root {}", catalog.display(), root.display());
            Ok(())
        }
        Cmd::Serve {
            catalog,
            root,
            listen,
            web,
        } => serve(catalog, root, listen, web),
        Cmd::Scan {
            catalog,
            operation,
            paths,
            depth,
        } => cli_scan(&catalog, &operation, &paths, depth),
        Cmd::Rebuild { catalog } => {
            let cat = Catalog::open(&catalog)?;
            let before = project::snapshot(&cat.conn)?;
            let t = Instant::now();
            project::rebuild(&cat.conn)?;
            let after = project::snapshot(&cat.conn)?;
            println!(
                "rebuilt {} occurrences in {:.2?}; {}",
                after.len(),
                t.elapsed(),
                if before == after { "identical to the incremental projection" } else { "DIFFERS from the incremental projection" }
            );
            Ok(())
        }
        Cmd::Suggest { catalog } => {
            let cat = Catalog::open(&catalog)?;
            for s in advise::suggestions(&cat)? {
                println!("[{}] {} ({} evidence item(s))", s.advisor, s.title, s.evidence_total);
                println!("    {}", s.reason);
                for e in s.evidence.iter().take(3) {
                    println!("    - {}: {}", e.path, e.detail);
                }
            }
            Ok(())
        }
    }
}

struct CliHooks(std::cell::Cell<Instant>);

impl Hooks for CliHooks {
    fn progress(&self, p: &Progress) {
        if self.0.get().elapsed() >= Duration::from_secs(1) {
            self.0.set(Instant::now());
            eprintln!(
                "  {} dirs, {} entries, {} files read, {:.1} MiB, {} problem(s) — {}",
                p.dirs_listed,
                p.entries_seen,
                p.files_read,
                p.bytes_read as f64 / 1048576.0,
                p.problems,
                p.current
            );
        }
    }
}

fn cli_scan(catalog: &Path, operation: &str, paths: &[PathBuf], depth: Option<u32>) -> Result<()> {
    let cat = Catalog::open(catalog)?;
    let operation = Operation::parse(operation)?;
    let mut targets = Vec::new();
    for p in paths {
        let bytes = p.as_os_str().as_bytes();
        let bytes = bytes.strip_prefix(b"./").unwrap_or(bytes);
        let bytes = if bytes == b"." { b"".as_slice() } else { bytes.strip_suffix(b"/").unwrap_or(bytes) };
        let is_dir = bytes.is_empty() || cat.root().join(p).symlink_metadata().map(|m| m.is_dir()).unwrap_or(false);
        targets.push(Target {
            path: pathenc::encode(bytes),
            kind: if is_dir { TargetKind::Dir } else { TargetKind::File },
            depth: if is_dir { depth } else { None },
        });
    }
    if targets.is_empty() {
        targets.push(Target {
            path: String::new(),
            kind: TargetKind::Dir,
            depth,
        });
    }
    let r = scan::submit(&cat.conn, &ScanRequest { operation, targets }, None)?;
    let t = Instant::now();
    let out = scan::run(&cat.conn, cat.root(), r.id, &CliHooks(std::cell::Cell::new(Instant::now())))?;
    println!(
        "scan #{} {}: {} in {:.2?}{}",
        out.scan_id,
        operation.as_str(),
        out.outcome,
        t.elapsed(),
        out.error.map(|e| format!(" — {e}")).unwrap_or_default()
    );
    println!("{}", serde_json::to_string_pretty(&out.summary["counts"])?);
    if out.summary["gaps"].as_u64().unwrap_or(0) > 0 {
        println!("gaps: {}", out.summary["gaps_by_code"]);
    }
    Ok(())
}

fn default_web_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/dist")
}

fn serve(catalog: PathBuf, root: Option<PathBuf>, listen: SocketAddr, web: Option<PathBuf>) -> Result<()> {
    if !Catalog::exists(&catalog) {
        let Some(root) = root else {
            bail!("{} has no catalog; pass --root to create one", catalog.display());
        };
        Catalog::init(&catalog, &root)?;
        eprintln!("initialized catalog {} for root {}", catalog.display(), root.display());
    } else if root.is_some() {
        eprintln!("note: --root is ignored; the catalog already has a root");
    }
    let cat = Catalog::open(&catalog)?;
    if cat.recovered > 0 {
        eprintln!("recovered {} interrupted scan(s) as abandoned", cat.recovered);
    }
    eprintln!("catalog {}  root {}", cat.dir().display(), cat.root().display());
    let handle = worker::spawn(cat)?;

    let web = web.unwrap_or_else(default_web_dir);
    let index = web.join("index.html");
    if !index.exists() {
        eprintln!(
            "warning: no web UI at {} (run `npm run build` in web/); serving the API only",
            web.display()
        );
    }
    let app = axum::Router::new()
        .nest("/api", api::router(handle.clone()))
        .fallback_service(ServeDir::new(&web).not_found_service(ServeFile::new(index)));

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(listen)
            .await
            .with_context(|| format!("binding {listen}"))?;
        eprintln!("Filestore is listening on http://{listen}/");
        let h = handle.clone();
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("installing a SIGTERM handler");
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
                shutdown(&h).await;
            })
            .await?;
        Ok(())
    })
}

/// On Ctrl-C or SIGTERM: cancel a running scan and wait briefly for it to
/// finalize, so completed work is published rather than left for recovery
/// as abandoned.
async fn shutdown(h: &worker::Handle) {
    let running = h.status().busy.map(|p| p.scan_id);
    if let Some(id) = running {
        eprintln!("cancelling scan #{id} before exit…");
        h.cancel(id);
        let deadline = Instant::now() + Duration::from_secs(30);
        while h.status().busy.is_some() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}
