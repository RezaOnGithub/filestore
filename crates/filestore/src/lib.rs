//! Filestore prototype core: catalog, synchronous scanner, evidence
//! interpretation (occurrence projection), advisors, and semantic records.
//!
//! Everything here is synchronous and single-connection. Callers serialize
//! access; the server does so with one worker thread that owns the catalog.

pub mod advise;
pub mod catalog;
pub mod error;
pub mod extract;
pub mod fsx;
pub mod ids;
pub mod ledger;
pub mod pathenc;
pub mod project;
pub mod query;
pub mod sample;
pub mod scan;
pub mod semantic;

pub use catalog::Catalog;
pub use error::{Error, Result};

/// Temporary directory for tests, under the repository's `.local/`.
#[cfg(test)]
pub(crate) fn test_tmp() -> tempfile::TempDir {
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.local/test-tmp");
    std::fs::create_dir_all(&base).unwrap();
    tempfile::Builder::new().prefix("unit").tempdir_in(base).unwrap()
}
