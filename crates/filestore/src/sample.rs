//! Sampled content fingerprints for Check.
//!
//! This prototype replaces DESIGN.md §6.3 (logarithmic byte budget, Floyd
//! selection from a BLAKE3 stream) with a simpler fixed-count policy:
//!
//! * The file is split into `block_size` blocks; `N = ceil(size / block_size)`.
//! * If `N <= count`, the plan covers the whole file and Check performs a
//!   full Verify instead (no baseline is needed or stored).
//! * Otherwise exactly `count` blocks are sampled at evenly spaced indices
//!   `j * (N - 1) / (count - 1)` for `j = 0..count`, which always includes the
//!   first and last block. Positions depend only on the policy and size.
//! * The token is `BLAKE3::derive_key("filestore sample token v1", payload)`
//!   over the policy encoding, size, block count, and for each block its
//!   offset, length, and bytes.
//!
//! Every sampled read costs at most `count * block_size` bytes (1 MiB by
//! default) and `count` seeks, independent of file size. Fixed positions keep
//! the usual blind spot: edits that avoid every sampled block and keep the
//! size unchanged are not detected.

use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SamplePolicy {
    pub block_size: u64,
    pub count: u64,
}

pub const TOKEN_CONTEXT: &str = "filestore sample token v1";

impl Default for SamplePolicy {
    fn default() -> Self {
        SamplePolicy {
            block_size: 64 * 1024,
            count: 16,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    /// Every byte is covered: perform a full read.
    Full,
    /// Sampled `(offset, length)` blocks in increasing offset order.
    Blocks(Vec<(u64, u64)>),
}

impl SamplePolicy {
    pub fn encoding(&self) -> String {
        format!(
            "filestore-sample/1;block={};count={}",
            self.block_size, self.count
        )
    }

    pub fn plan(&self, size: u64) -> Plan {
        assert!(self.block_size > 0 && self.count >= 2);
        let n = size.div_ceil(self.block_size);
        if n <= self.count {
            return Plan::Full;
        }
        let blocks = (0..self.count)
            .map(|j| {
                let index = j * (n - 1) / (self.count - 1);
                let offset = index * self.block_size;
                (offset, self.block_size.min(size - offset))
            })
            .collect();
        Plan::Blocks(blocks)
    }

    /// Bytes a sampled Check reads for a file of this size.
    pub fn sampled_bytes(&self, size: u64) -> u64 {
        match self.plan(size) {
            Plan::Full => size,
            Plan::Blocks(b) => b.iter().map(|(_, l)| l).sum(),
        }
    }
}

/// Incremental token computation. Accepts bytes either streamed in order
/// from offset zero (during a full read) or block by block (during Check);
/// both produce the same token.
pub struct TokenHasher {
    hasher: blake3::Hasher,
    blocks: Vec<(u64, u64)>,
    next: usize,
    started: bool,
    broken: bool,
}

impl TokenHasher {
    pub fn new(policy: &SamplePolicy, size: u64, blocks: Vec<(u64, u64)>) -> TokenHasher {
        let mut hasher = blake3::Hasher::new_derive_key(TOKEN_CONTEXT);
        let enc = policy.encoding();
        hasher.update(&(enc.len() as u64).to_le_bytes());
        hasher.update(enc.as_bytes());
        hasher.update(&size.to_le_bytes());
        hasher.update(&(blocks.len() as u64).to_le_bytes());
        TokenHasher {
            hasher,
            blocks,
            next: 0,
            started: false,
            broken: false,
        }
    }

    /// Feed `data` located at absolute offset `offset`. Calls must be in
    /// increasing, non-overlapping offset order.
    pub fn feed(&mut self, offset: u64, data: &[u8]) {
        let end = offset + data.len() as u64;
        while self.next < self.blocks.len() {
            let (bo, bl) = self.blocks[self.next];
            let be = bo + bl;
            if bo >= end {
                break;
            }
            if be <= offset {
                // A selected block was skipped entirely.
                self.broken = true;
                self.next += 1;
                self.started = false;
                continue;
            }
            if !self.started {
                if offset > bo {
                    self.broken = true;
                }
                self.hasher.update(&bo.to_le_bytes());
                self.hasher.update(&bl.to_le_bytes());
                self.started = true;
            }
            let s = bo.max(offset);
            let e = be.min(end);
            self.hasher
                .update(&data[(s - offset) as usize..(e - offset) as usize]);
            if e == be {
                self.next += 1;
                self.started = false;
            } else {
                break;
            }
        }
    }

    /// The token, or `None` if not every selected byte was fed exactly once.
    pub fn finish(self) -> Option<[u8; 32]> {
        if self.broken || self.next != self.blocks.len() {
            return None;
        }
        Some(*self.hasher.finalize().as_bytes())
    }
}

/// Read the selected blocks of `file` (coalescing adjacent ones) and compute
/// the token. Returns the token and the number of bytes read.
pub fn sample_file(
    file: &File,
    policy: &SamplePolicy,
    size: u64,
    blocks: &[(u64, u64)],
) -> io::Result<([u8; 32], u64)> {
    let mut hasher = TokenHasher::new(policy, size, blocks.to_vec());
    let mut read = 0u64;
    let mut i = 0;
    while i < blocks.len() {
        let start = blocks[i].0;
        let mut end = blocks[i].0 + blocks[i].1;
        let mut j = i + 1;
        while j < blocks.len() && blocks[j].0 == end {
            end += blocks[j].1;
            j += 1;
        }
        let mut buf = vec![0u8; (end - start) as usize];
        file.read_exact_at(&mut buf, start)?;
        read += buf.len() as u64;
        hasher.feed(start, &buf);
        i = j;
    }
    let token = hasher
        .finish()
        .ok_or_else(|| io::Error::other("sample plan was not fully read"))?;
    Ok((token, read))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn small_files_are_full_reads() {
        let p = SamplePolicy::default();
        assert_eq!(p.plan(0), Plan::Full);
        assert_eq!(p.plan(1), Plan::Full);
        assert_eq!(p.plan(16 * 65536), Plan::Full);
        assert!(matches!(p.plan(16 * 65536 + 1), Plan::Blocks(_)));
    }

    #[test]
    fn plan_includes_endpoints_and_is_distinct() {
        let p = SamplePolicy::default();
        for size in [16 * 65536 + 1, 17 * 65536, 10_000_000, 1 << 40, (1 << 40) + 7] {
            let Plan::Blocks(b) = p.plan(size) else {
                panic!()
            };
            assert_eq!(b.len(), 16);
            assert_eq!(b[0].0, 0);
            let (lo, ll) = *b.last().unwrap();
            assert_eq!(lo + ll, size, "last block ends at EOF");
            assert!(b.windows(2).all(|w| w[0].0 + w[0].1 <= w[1].0));
            assert!(p.sampled_bytes(size) <= 16 * 65536);
        }
    }

    #[test]
    fn streamed_and_sampled_tokens_agree() {
        let p = SamplePolicy {
            block_size: 4096,
            count: 4,
        };
        let data: Vec<u8> = (0..50_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let Plan::Blocks(blocks) = p.plan(data.len() as u64) else {
            panic!()
        };

        // Streamed in odd-sized chunks.
        let mut streamed = TokenHasher::new(&p, data.len() as u64, blocks.clone());
        for (k, chunk) in data.chunks(3001).enumerate() {
            streamed.feed(k as u64 * 3001, chunk);
        }
        let t1 = streamed.finish().unwrap();

        let dir = crate::test_tmp();
        let path = dir.path().join("f");
        File::create(&path).unwrap().write_all(&data).unwrap();
        let f = File::open(&path).unwrap();
        let (t2, read) = sample_file(&f, &p, data.len() as u64, &blocks).unwrap();
        assert_eq!(t1, t2);
        assert_eq!(read, blocks.iter().map(|b| b.1).sum::<u64>());

        // A change in a sampled block changes the token; one outside does not.
        let mut changed = data.clone();
        changed[0] ^= 1;
        let mut h = TokenHasher::new(&p, data.len() as u64, blocks.clone());
        h.feed(0, &changed);
        assert_ne!(h.finish().unwrap(), t1);
        let unsampled = (0..data.len() as u64)
            .find(|o| !blocks.iter().any(|(b, l)| o >= b && *o < b + l))
            .unwrap() as usize;
        let mut blind = data.clone();
        blind[unsampled] ^= 1;
        let mut h = TokenHasher::new(&p, data.len() as u64, blocks);
        h.feed(0, &blind);
        assert_eq!(h.finish().unwrap(), t1, "the fixed-sample blind spot");
    }

    #[test]
    fn different_policies_give_different_tokens() {
        let a = SamplePolicy {
            block_size: 4096,
            count: 4,
        };
        let b = SamplePolicy {
            block_size: 4096,
            count: 5,
        };
        let data = vec![1u8; 100_000];
        let tok = |p: &SamplePolicy| {
            let Plan::Blocks(bl) = p.plan(data.len() as u64) else {
                panic!()
            };
            let mut h = TokenHasher::new(p, data.len() as u64, bl);
            h.feed(0, &data);
            h.finish().unwrap()
        };
        assert_ne!(tok(&a), tok(&b));
    }
}
