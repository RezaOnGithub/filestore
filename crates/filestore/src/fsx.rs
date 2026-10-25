//! Linux filesystem primitives used by the scanner.
//!
//! All paths below the root are resolved with `openat2` from an open root
//! handle, constrained to stay beneath it, never follow symlinks, and never
//! cross mounts. There is no fallback to unconstrained opens.

use std::ffi::{CStr, CString, OsStr};
use std::fs::File;
use std::io;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use rustix::fs::{
    AtFlags, Dir, FileType, Mode, OFlags, ResolveFlags, Stat, StatxAttributes, StatxFlags, fstat,
    fstatfs, openat2, statx,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    Symlink,
    Other,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::File => "file",
            Kind::Dir => "dir",
            Kind::Symlink => "symlink",
            Kind::Other => "other",
        }
    }

    fn from_mode(mode: u32) -> Kind {
        match FileType::from_raw_mode(mode) {
            FileType::RegularFile => Kind::File,
            FileType::Directory => Kind::Dir,
            FileType::Symlink => Kind::Symlink,
            _ => Kind::Other,
        }
    }
}

/// Filesystem metadata measured from one handle or directory entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Meta {
    pub kind: Kind,
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime_ns: i64,
    pub ctime_ns: i64,
    pub mode: u32,
    pub nlink: u64,
    /// The entry is the root of a mount (including a bind mount from the
    /// same filesystem, which has the same device number).
    pub mount_root: bool,
}

/// Nanoseconds since the epoch, saturating instead of overflowing for
/// timestamps beyond the years 1677–2262.
fn nanos(sec: i64, nsec: i64) -> i64 {
    sec.saturating_mul(1_000_000_000).saturating_add(nsec)
}

impl Meta {
    fn from_stat(st: &Stat) -> Meta {
        Meta {
            kind: Kind::from_mode(st.st_mode),
            dev: st.st_dev,
            ino: st.st_ino,
            size: st.st_size as u64,
            mtime_ns: nanos(st.st_mtime, st.st_mtime_nsec as i64),
            ctime_ns: nanos(st.st_ctime, st.st_ctime_nsec as i64),
            mode: st.st_mode,
            nlink: st.st_nlink,
            mount_root: false,
        }
    }

    /// Same native object with unchanged size and timestamps.
    pub fn stable_against(&self, other: &Meta) -> bool {
        self.dev == other.dev
            && self.ino == other.ino
            && self.size == other.size
            && self.mtime_ns == other.mtime_ns
            && self.ctime_ns == other.ctime_ns
    }
}

const RESOLVE: ResolveFlags = ResolveFlags::BENEATH
    .union(ResolveFlags::NO_SYMLINKS)
    .union(ResolveFlags::NO_XDEV)
    .union(ResolveFlags::NO_MAGICLINKS);

pub fn open_root(path: &Path) -> io::Result<OwnedFd> {
    let f = File::open(path)?;
    let fd: OwnedFd = f.into();
    let st = fstat(&fd)?;
    if Kind::from_mode(st.st_mode) != Kind::Dir {
        return Err(io::Error::from_raw_os_error(libc::ENOTDIR));
    }
    Ok(fd)
}

fn c_path(rel: &[u8]) -> io::Result<CString> {
    let rel = if rel.is_empty() { b".".as_slice() } else { rel };
    CString::new(rel).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))
}

fn open_beneath(root: &OwnedFd, rel: &[u8], flags: OFlags) -> io::Result<OwnedFd> {
    let path = c_path(rel)?;
    let mut tries = 0;
    loop {
        match openat2(root, path.as_c_str(), flags | OFlags::CLOEXEC, Mode::empty(), RESOLVE) {
            // EAGAIN signals a concurrent rename during constrained lookup.
            Err(e) if e == rustix::io::Errno::AGAIN && tries < 3 => tries += 1,
            r => return r.map_err(io::Error::from),
        }
    }
}

pub fn open_dir(root: &OwnedFd, rel: &[u8]) -> io::Result<OwnedFd> {
    open_beneath(root, rel, OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW)
}

/// Open for reading. `NONBLOCK` keeps a FIFO that raced into place from
/// blocking the open; callers check the handle's type before reading.
pub fn open_file(root: &OwnedFd, rel: &[u8]) -> io::Result<File> {
    let fd = open_beneath(
        root,
        rel,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::NOCTTY,
    )?;
    Ok(File::from(fd))
}

/// Resolve a path without opening its content and measure what it denotes.
pub fn locate(root: &OwnedFd, rel: &[u8]) -> io::Result<Meta> {
    let fd = open_beneath(root, rel, OFlags::PATH | OFlags::NOFOLLOW)?;
    meta_of(&fd)
}

pub fn meta_of<Fd: AsFd>(fd: Fd) -> io::Result<Meta> {
    Ok(Meta::from_stat(&fstat(fd)?))
}

/// Measure a directory entry without following it. Uses `statx` so mount
/// roots are recognized even when they share the parent's device number.
pub fn meta_at<Fd: AsFd>(dir: Fd, name: &CStr) -> io::Result<Meta> {
    let st = statx(&dir, name, AtFlags::SYMLINK_NOFOLLOW, StatxFlags::BASIC_STATS)?;
    let mode = u32::from(st.stx_mode);
    Ok(Meta {
        kind: Kind::from_mode(mode),
        dev: rustix::fs::makedev(st.stx_dev_major, st.stx_dev_minor),
        ino: st.stx_ino,
        size: st.stx_size,
        mtime_ns: nanos(st.stx_mtime.tv_sec, i64::from(st.stx_mtime.tv_nsec)),
        ctime_ns: nanos(st.stx_ctime.tv_sec, i64::from(st.stx_ctime.tv_nsec)),
        mode,
        nlink: u64::from(st.stx_nlink),
        mount_root: st.stx_attributes_mask.contains(StatxAttributes::MOUNT_ROOT)
            && st.stx_attributes.contains(StatxAttributes::MOUNT_ROOT),
    })
}

pub struct DirEntry {
    pub name: Vec<u8>,
}

/// List a directory's immediate entries (excluding `.` and `..`). On a read
/// error the entries read so far are returned with the error.
pub fn list_dir(fd: &OwnedFd) -> (Vec<DirEntry>, Option<io::Error>) {
    let mut out = Vec::new();
    let mut dir = match Dir::read_from(fd) {
        Ok(d) => d,
        Err(e) => return (out, Some(e.into())),
    };
    while let Some(entry) = dir.read() {
        match entry {
            Ok(e) => {
                let name = e.file_name().to_bytes();
                if name == b"." || name == b".." {
                    continue;
                }
                out.push(DirEntry {
                    name: name.to_vec(),
                });
            }
            Err(e) => return (out, Some(e.into())),
        }
    }
    (out, None)
}

pub fn cstr(name: &[u8]) -> io::Result<CString> {
    CString::new(name).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))
}

/// Short symbolic name for an I/O error, e.g. `EACCES`.
pub fn errno_name(e: &io::Error) -> String {
    let name = match e.raw_os_error() {
        Some(libc::EACCES) => "EACCES",
        Some(libc::EPERM) => "EPERM",
        Some(libc::ENOENT) => "ENOENT",
        Some(libc::ENOTDIR) => "ENOTDIR",
        Some(libc::EISDIR) => "EISDIR",
        Some(libc::ELOOP) => "ELOOP",
        Some(libc::EXDEV) => "EXDEV",
        Some(libc::EIO) => "EIO",
        Some(libc::EAGAIN) => "EAGAIN",
        Some(libc::ENAMETOOLONG) => "ENAMETOOLONG",
        Some(libc::EINVAL) => "EINVAL",
        Some(libc::ESTALE) => "ESTALE",
        Some(libc::ENXIO) => "ENXIO",
        Some(n) => return format!("errno {n}"),
        None => return e.to_string(),
    };
    name.to_string()
}

/// Persistent identity of the filesystem holding `fd`: its type, the statfs
/// filesystem ID, and the UUID and subvolume where the kernel reports them.
/// Excludes device numbers and mount IDs, which change across reboots.
/// Compare contexts with [`same_context`], not as strings: which optional
/// parts appear depends on the kernel version.
pub fn fs_context(fd: &OwnedFd) -> String {
    let (fs_type, fsid) = match fstatfs(fd) {
        Ok(s) => {
            // SAFETY: Fsid is two c_ints; reinterpret them as raw bytes.
            let raw: [i32; 2] = unsafe { std::mem::transmute_copy(&s.f_fsid) };
            let fsid = format!("{:08x}{:08x}", raw[0] as u32, raw[1] as u32);
            #[allow(clippy::unnecessary_cast)] // f_type's width varies by platform
            (fs_type_name(s.f_type as i64), fsid)
        }
        Err(_) => ("unknown".to_string(), "unknown".to_string()),
    };
    let mut ctx = format!("{fs_type} fsid={fsid}");
    if let Some(uuid) = fs_uuid(fd) {
        ctx.push_str(&format!(" uuid={uuid}"));
    }
    if let Some(subvol) = subvolume(fd) {
        ctx.push_str(&format!(" subvol={subvol}"));
    }
    ctx
}

/// Whether two recorded contexts denote the same filesystem. The types must
/// agree; UUIDs decide when both have one (some filesystems derive the
/// statfs ID from the device number), otherwise the statfs IDs do; and
/// subvolumes must agree when both are known. Parts that only one side has
/// (a newer kernel reports more) are ignored.
pub fn same_context(a: &str, b: &str) -> bool {
    fn parse(s: &str) -> (&str, std::collections::HashMap<&str, &str>) {
        let mut it = s.split_whitespace();
        let ty = it.next().unwrap_or("");
        (ty, it.filter_map(|kv| kv.split_once('=')).collect())
    }
    let (ta, a) = parse(a);
    let (tb, b) = parse(b);
    if ta != tb {
        return false;
    }
    let both = |k: &str| a.get(k).zip(b.get(k));
    let identity = match both("uuid") {
        Some((x, y)) => x == y,
        None => matches!(both("fsid"), Some((x, y)) if x == y),
    };
    identity && both("subvol").is_none_or(|(x, y)| x == y)
}

/// `FS_IOC_GETFSUUID` (Linux 6.7+): `_IOR(0x15, 0, struct fsuuid2)`.
fn fs_uuid(fd: &OwnedFd) -> Option<String> {
    #[repr(C)]
    struct FsUuid2 {
        len: u8,
        uuid: [u8; 16],
    }
    const FS_IOC_GETFSUUID: libc::c_ulong = 0x8011_1500;
    let mut out = FsUuid2 {
        len: 0,
        uuid: [0; 16],
    };
    // SAFETY: the kernel writes at most size_of::<FsUuid2>() bytes into `out`.
    let rc = unsafe { libc::ioctl(fd.as_raw_fd(), FS_IOC_GETFSUUID, &mut out as *mut FsUuid2) };
    if rc != 0 || out.len == 0 || out.len > 16 {
        return None;
    }
    let bytes = &out.uuid[..out.len as usize];
    if bytes.iter().all(|&b| b == 0) {
        return None;
    }
    Some(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Subvolume ID via `STATX_SUBVOL` (Linux 6.10+), when the filesystem has one.
fn subvolume(fd: &OwnedFd) -> Option<u64> {
    const STATX_SUBVOL: u32 = 0x8000;
    let st = statx(
        fd,
        OsStr::from_bytes(b""),
        AtFlags::EMPTY_PATH,
        StatxFlags::from_bits_retain(STATX_SUBVOL),
    )
    .ok()?;
    (st.stx_mask & STATX_SUBVOL != 0).then_some(st.stx_subvol)
}

fn fs_type_name(magic: i64) -> String {
    match magic as u32 {
        0x9123_683E => "btrfs".into(),
        0xEF53 => "ext4".into(),
        0x5846_5342 => "xfs".into(),
        0x0102_1994 => "tmpfs".into(),
        0x2FC1_2FC1 => "zfs".into(),
        0xF2F5_2010 => "f2fs".into(),
        0xCA45_1A4E => "bcachefs".into(),
        0x794C_7630 => "overlayfs".into(),
        0x6573_5546 => "fuse".into(),
        0x6969 => "nfs".into(),
        other => format!("fs-{other:08x}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contexts_compare_structurally() {
        let old = "btrfs fsid=e2b2 subvol=257";
        assert!(same_context(old, "btrfs fsid=e2b2 uuid=aa subvol=257"), "a newer kernel adds a UUID");
        assert!(same_context("btrfs fsid=e2b2", old), "an older kernel lacks the subvolume");
        assert!(!same_context(old, "btrfs fsid=e2b2 subvol=258"));
        assert!(!same_context(old, "ext4 fsid=e2b2"));
        assert!(same_context("xfs fsid=01 uuid=u1", "xfs fsid=02 uuid=u1"), "the UUID decides");
        assert!(!same_context("xfs fsid=01 uuid=u1", "xfs fsid=01 uuid=u2"));
        assert!(!same_context("ext4 uuid=u1", "ext4 fsid=01"), "nothing comparable");
    }
}
