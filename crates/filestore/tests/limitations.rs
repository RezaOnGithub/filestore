//! Known simplifications, pinned down so that changing them is deliberate.
//! See doc/IMPLEMENTATION.md for the reasoning and impact.

mod common;

use std::cell::Cell;
use std::fs;
use std::path::PathBuf;

use common::{big, dir, env, file};
use filestore::scan::{Hooks, Operation};

/// Replaces directory `d` with a new directory that receives the same file
/// object, after `d` was listed and before the file is opened.
struct SwapParent {
    root: PathBuf,
    calls: Cell<u32>,
}

impl Hooks for SwapParent {
    fn cancelled(&self) -> bool {
        // Calls: target, root listing, d listing, file loop, verify attempt.
        let n = self.calls.get() + 1;
        self.calls.set(n);
        if n == 4 {
            let d = self.root.join("d");
            fs::rename(d.join("f.bin"), self.root.join("f.tmp")).unwrap();
            fs::rename(&d, self.root.join("d.old")).unwrap();
            fs::create_dir(&d).unwrap();
            fs::rename(self.root.join("f.tmp"), d.join("f.bin")).unwrap();
        }
        false
    }
}

/// The location check after a read compares only the final object, not each
/// ancestor. A read of the same file object through a replaced parent
/// directory is confirmed and attached under the parent instance this scan
/// listed. The next observation of the parent ends that instance; the file
/// becomes a new, unidentified occurrence that Check or Verify re-establishes.
#[test]
fn ancestor_replacement_during_a_read_is_corrected_by_the_next_scan() {
    let e = env();
    e.write("d/f.bin", &big(3, 3 * 1024 * 1024));
    e.scan(Operation::Inventory, vec![dir("", None)]);
    let old_dir = e.state("d").unwrap().ino;

    let r = e.scan_with(Operation::Verify, vec![dir("", None)], &SwapParent { root: e.root.clone(), calls: Cell::new(0) });
    assert_eq!(r.outcome, "succeeded", "the swap is not detected: {:#}", r.summary);
    assert_eq!(e.state("d").unwrap().ino, old_dir, "still the listed (old) directory instance");
    let f = e.state("d/f.bin").unwrap();
    assert_eq!(f.content, "verified", "attached under the old instance");
    assert_ne!(e.ino("d"), old_dir.unwrap());

    // Any later observation of the parent reveals the replacement.
    e.scan(Operation::Inventory, vec![dir("", None)]);
    assert_eq!(e.ended("d"), vec!["replaced"]);
    assert_eq!(e.ended("d/f.bin"), vec!["ancestor_ended"]);
    let f2 = e.state("d/f.bin").unwrap();
    assert_eq!(f2.content, "unidentified", "verification does not carry over to the new instance");

    // Check compares the file with the content last verified at the path.
    e.scan(Operation::Check, vec![file("d/f.bin")]);
    let f3 = e.state("d/f.bin").unwrap();
    assert_eq!(f3.content, "plausible");
    assert_eq!(f3.blob, f.blob);
    e.assert_rebuild_matches();
}
