//! **Windows-only** worktree metadata caching. During per-entry modification
//! checks, parent directories can be enumerated lazily so that
//! `index_as_worktree` can reuse stat results instead of issuing a per-file
//! `lstat`. This trade only pays off where per-file stat is expensive
//! and where the dirwalk returns basic stat information, so only on Windows.
//!
//! [`FsCache`] lazily enumerates parent directories with
//! `GetFileInformationByHandleEx` and keeps the results in the status worker
//! that asked for them. It is not a long-lived cache: it is built during one
//! status call and discarded with the worker state. Lookups are transparent:
//! misses fall through to a live syscall, so cache misses affect speed only.

use std::path::{Path, PathBuf};

use bstr::{BStr, BString, ByteSlice};

/// File metadata produced by the lazy cache for one worktree entry.
///
/// Carries enough information to determine file type, detect mode changes,
/// build a [`gix_index::entry::Stat`] for comparison, and short-circuit content
/// reads via file size.
///
/// Windows-only fields: this module is `#[cfg(windows)]`, and Windows batch
/// directory enumeration doesn't expose `dev`/`ino`/`uid`/`gid` or the
/// executable bit. The status pipeline's stat comparison on Windows compares
/// those `Stat` fields against matching zeros from
/// [`gix_index::entry::Stat::from_fs`]'s Windows branch, and git on Windows
/// defaults to `core.filemode=false`, so all five are simply omitted here.
#[derive(Debug, Clone, Copy, Default)]
pub struct Stat {
    /// Whether this is a directory, with the same semantics as
    /// `std::fs::symlink_metadata`: `false` for directory symlinks and junctions
    /// even though the filesystem also marks those with the directory attribute.
    pub is_dir: bool,
    /// Whether this is a symlink, with the same semantics as
    /// `std::fs::symlink_metadata`: `true` only for name-surrogate reparse points
    /// (symlinks and junctions/mount points). Other reparse points — OneDrive
    /// cloud placeholders, ProjFS, app-exec links — are regular files/dirs, as
    /// the live `lstat` fallback would report them.
    pub is_symlink: bool,
    /// File size in bytes.
    pub size: u64,
    /// Modification time — seconds since Unix epoch.
    pub mtime_secs: u32,
    /// Modification time — nanoseconds component.
    pub mtime_nsecs: u32,
    /// Status/creation time — seconds since Unix epoch.
    ///
    /// On Windows this must be populated from the real `CreationTime`, not `mtime`:
    /// the stat comparison in the status pipeline compares `ctime.secs` by default
    /// (`trust_ctime=true`), and faking `ctime=mtime` causes spurious mismatches
    /// for any file whose creation-time and modification-time differ.
    pub ctime_secs: u32,
    /// Status/creation time — nanoseconds component.
    pub ctime_nsecs: u32,
}

impl Stat {
    /// Convert to gitoxide's [`Stat`](gix_index::entry::Stat) struct for index comparison.
    ///
    /// Truncates `size` from 64 to 32 bits — matching what
    /// [`gix_index::entry::stat::Stat::from_fs`] does on Unix, so both code
    /// paths compare the same quantities. `dev`/`ino`/`uid`/`gid` are zeroed
    /// here to match what `from_fs` produces on Windows.
    pub(crate) fn to_stat(&self) -> gix_index::entry::Stat {
        gix_index::entry::Stat {
            mtime: gix_index::entry::stat::Time {
                secs: self.mtime_secs,
                nsecs: self.mtime_nsecs,
            },
            ctime: gix_index::entry::stat::Time {
                secs: self.ctime_secs,
                nsecs: self.ctime_nsecs,
            },
            dev: 0,
            ino: 0,
            uid: 0,
            gid: 0,
            size: self.size as u32,
        }
    }
}

/// Metadata for one enumerated directory, keyed by direct filename.
type DirectoryEntries = hashbrown::HashMap<BString, Stat>;

/// Either a live `lstat` result or a precomputed [`Stat`] from the lazy
/// cache. Lets [`crate::index_as_worktree`] treat both shapes uniformly without
/// branching at every per-entry use site.
pub(crate) enum Metadata {
    Live(gix_index::fs::Metadata),
    Cached(Stat),
}

impl Metadata {
    pub(crate) fn is_dir(&self) -> bool {
        match self {
            Self::Live(m) => m.is_dir(),
            Self::Cached(c) => c.is_dir,
        }
    }

    pub(crate) fn is_symlink(&self) -> bool {
        match self {
            Self::Live(m) => m.is_symlink(),
            Self::Cached(c) => c.is_symlink,
        }
    }

    pub(crate) fn len(&self) -> u64 {
        match self {
            Self::Live(m) => m.len(),
            Self::Cached(c) => c.size,
        }
    }

    pub(crate) fn to_stat(&self) -> Result<gix_index::entry::Stat, std::time::SystemTimeError> {
        match self {
            Self::Live(m) => gix_index::entry::Stat::from_fs(m),
            Self::Cached(c) => Ok(c.to_stat()),
        }
    }

    pub(crate) fn mode_change(
        &self,
        entry_mode: gix_index::entry::Mode,
        has_symlinks: bool,
        executable_bit: bool,
    ) -> Option<gix_index::entry::mode::Change> {
        match self {
            Self::Live(m) => entry_mode.change_to_match_fs(m, has_symlinks, executable_bit),
            // Windows batch enumeration doesn't expose the executable bit; pass `false`.
            // Git on Windows defaults to `core.filemode=false` so this is unused anyway.
            // TODO(correctness): it seems we'd have to try to support this at some point,
            //                    depending on Git for Windows' implementation.
            Self::Cached(c) => entry_mode.change_to_match_fs_with_values(
                !c.is_dir && !c.is_symlink, // is_file: regular file (not dir, not symlink)
                c.is_dir,
                c.is_symlink,
                false,
                has_symlinks,
                executable_bit,
            ),
        }
    }
}

/// Thread-local, lazily populated worktree metadata cache.
///
/// Each lookup caches the entry's parent directory, keyed by the worktree
/// relative directory path. Directory entries are keyed by filename only. This
/// keeps the cache independent from pathspecs and excludes while avoiding a
/// live `lstat` for every tracked file in directories that have already been
/// enumerated.
pub struct FsCache {
    /// Absolute worktree root used to turn repository-relative directory keys
    /// into filesystem paths for Windows directory enumeration.
    ///
    /// The cache never changes this root and never stores absolute paths as
    /// keys. Keeping the root separate lets lookups operate on Git-style
    /// forward-slashed paths while `directory_entries()` performs the
    /// platform-specific conversion at the boundary.
    worktree: PathBuf,
    /// Cache of direct directory listings, keyed by worktree-relative parent
    /// directory path.
    ///
    /// Keys use the same byte representation as index paths: forward slashes,
    /// no leading slash, and an empty key for the worktree root. Values are
    /// `Some(entries)` when that directory was enumerated successfully and
    /// `None` when enumeration failed, for example because the directory was
    /// removed or inaccessible. Caching failed enumerations prevents repeated
    /// directory-open attempts for multiple tracked entries in the same missing
    /// directory; callers still fall through to live per-file metadata and
    /// preserve correctness.
    ///
    /// `DirectoryEntries` itself is keyed only by direct filename, not by full
    /// worktree-relative path. This keeps each cached listing small and mirrors
    /// the Windows API result shape: first resolve the parent directory, then
    /// look up the requested basename in that listing.
    directories: hashbrown::HashMap<BString, Option<DirectoryEntries>>,
}

impl FsCache {
    /// Create an empty cache rooted at `worktree`.
    pub fn new(worktree: &Path) -> Self {
        FsCache {
            worktree: worktree.to_owned(),
            directories: Default::default(),
        }
    }

    /// Return cached metadata for `rela_path`, populating its parent directory
    /// on first use. `None` means the cache couldn't help and callers should
    /// fall back to a live `lstat`.
    pub(crate) fn get(&mut self, rela_path: &BStr) -> Option<Stat> {
        let (dir, filename) = match rela_path.rfind_byte(b'/') {
            Some(pos) => (&rela_path[..pos], &rela_path[pos + 1..]),
            None => (BStr::new(b""), rela_path),
        };
        if filename.is_empty() {
            return None;
        }

        if let Some(entries) = self.directories.get(dir) {
            return entries.as_ref().and_then(|entries| entries.get(filename)).copied();
        }

        let entries = windows::directory_entries(&self.worktree, dir.into());
        let stat = entries.as_ref().and_then(|entries| entries.get(filename)).copied();
        self.directories.insert(dir.into(), entries);
        stat
    }
}

mod windows {
    use super::*;
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_ID_BOTH_DIR_INFO, FILE_LIST_DIRECTORY, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        FileIdBothDirectoryInfo, GetFileInformationByHandleEx, OPEN_EXISTING, SYNCHRONIZE,
    };

    /// Convert FILE_ID_BOTH_DIR_INFO to a [`Stat`].
    ///
    /// File-type flags must mirror what the live fallback
    /// (`gix_index::fs::Metadata`, i.e. `std::fs::symlink_metadata`) reports for
    /// the same path, or cache hits and misses would disagree on the type and
    /// misreport unchanged entries as type-changed or removed. std only calls a
    /// reparse point a symlink if its tag is a *name surrogate* (symlinks and
    /// junctions), and reports `is_dir=false` for those; non-surrogate reparse
    /// points (OneDrive cloud placeholders, ProjFS, app-exec links, WOF) are
    /// plain files/directories to std. Directory enumeration stores the reparse
    /// tag in `EaSize` when `FILE_ATTRIBUTE_REPARSE_POINT` is set (MS-FSCC
    /// 2.4.17, same convention as `WIN32_FIND_DATA::dwReserved0`), so we can
    /// replicate that logic without extra syscalls.
    fn stat_from_info(info: &FILE_ID_BOTH_DIR_INFO) -> Stat {
        let size = info.EndOfFile as u64;

        // FILETIME values are LARGE_INTEGER holding 100ns intervals since 1601-01-01 UTC.
        // `ctime` must come from `CreationTime`: `gix_index::entry::stat::from_fs`
        // on Windows populates ctime from `Metadata::created()`, which is CreationTime.
        let (mtime_secs, mtime_nsecs) = filetime_to_unix(info.LastWriteTime as u64);
        let (ctime_secs, ctime_nsecs) = filetime_to_unix(info.CreationTime as u64);

        let (is_dir, is_symlink) = type_flags_from_attributes(info.FileAttributes, info.EaSize);
        Stat {
            is_dir,
            is_symlink,
            size,
            mtime_secs,
            mtime_nsecs,
            ctime_secs,
            ctime_nsecs,
        }
    }

    /// Derive `(is_dir, is_symlink)` exactly like `std::fs::FileType` does for
    /// `symlink_metadata`, given raw directory-enumeration data. `reparse_tag`
    /// is `EaSize` reinterpreted, only meaningful when the reparse attribute is set.
    pub(super) fn type_flags_from_attributes(attributes: u32, reparse_tag: u32) -> (bool, bool) {
        /// `IsReparseTagNameSurrogate`: the tag names another filesystem object
        /// (symlink, junction/mount point), as opposed to tags that overlay data
        /// onto the file itself (cloud placeholders, WOF, ProjFS, ...).
        const NAME_SURROGATE_BIT: u32 = 0x2000_0000;
        let is_reparse = attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0;
        let is_symlink = is_reparse && (reparse_tag & NAME_SURROGATE_BIT) != 0;
        let is_dir = !is_symlink && (attributes & FILE_ATTRIBUTE_DIRECTORY) != 0;
        (is_dir, is_symlink)
    }

    /// Read a record's UTF-16 `FileName` without assuming the record is aligned.
    ///
    /// `FILE_ID_BOTH_DIR_INFO` records can be returned at unaligned offsets by some
    /// filesystem drivers (see the call site and rust-lang/rust#104530), so forming a
    /// `&[u16]` over the buffer would be UB. Borrow in place in the common aligned case;
    /// copy element-by-element with unaligned reads otherwise. Mirrors std's
    /// `from_maybe_unaligned` helper.
    ///
    /// # Safety
    ///
    /// `p` must point at `len` `u16`s of an in-bounds, initialized directory record.
    #[allow(unsafe_code)]
    unsafe fn name_from_maybe_unaligned<'a>(p: *const u16, len: usize) -> std::borrow::Cow<'a, [u16]> {
        if p.is_aligned() {
            std::borrow::Cow::Borrowed(unsafe { std::slice::from_raw_parts(p, len) })
        } else {
            std::borrow::Cow::Owned((0..len).map(|i| unsafe { p.add(i).read_unaligned() }).collect())
        }
    }

    /// Convert a Windows FILETIME (100ns intervals since 1601-01-01 UTC) to Unix (secs, nsecs).
    fn filetime_to_unix(ft: u64) -> (u32, u32) {
        const EPOCH_DIFF: u64 = 116_444_736_000_000_000;
        let unix_100ns = ft.saturating_sub(EPOCH_DIFF);
        let secs = (unix_100ns / 10_000_000) as u32;
        let nsecs = ((unix_100ns % 10_000_000) * 100) as u32;
        (secs, nsecs)
    }

    /// Convert a filesystem path into a null-terminated UTF-16 buffer suitable for `CreateFileW`.
    fn utf16_null_terminated(path: &Path) -> Vec<u16> {
        let mut v: Vec<u16> = path.as_os_str().encode_wide().collect();
        v.push(0);
        v
    }

    /// Read the direct entries in `rel_dir`, keyed by filename.
    ///
    /// Returns `None` if the directory can't be read. Status callers treat that
    /// as a cache miss and use the live syscall path for the requested entry.
    pub(super) fn directory_entries(worktree: &Path, rel_dir: &BStr) -> Option<DirectoryEntries> {
        let dir = if rel_dir.is_empty() {
            worktree.to_owned()
        } else {
            worktree.join(gix_path::from_bstr(rel_dir))
        };
        let dir_path = utf16_null_terminated(&dir);
        #[allow(unsafe_code)]
        let handle = unsafe {
            CreateFileW(
                dir_path.as_ptr(),
                FILE_LIST_DIRECTORY | SYNCHRONIZE,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                std::ptr::null_mut(),
            )
        };

        if handle == INVALID_HANDLE_VALUE {
            return None;
        }

        let mut entries = DirectoryEntries::default();
        // 64 KiB, u64-aligned: `FILE_ID_BOTH_DIR_INFO` contains LARGE_INTEGER
        // fields that require 8-byte alignment, and `Vec<u64>` guarantees it.
        const DIRECTORY_BUFFER_U64_WORDS: usize = 8 * 1024;
        let mut buffer = vec![0u64; DIRECTORY_BUFFER_U64_WORDS];
        let buffer_bytes = (buffer.len() * 8) as u32;

        loop {
            #[allow(unsafe_code)]
            let success = unsafe {
                GetFileInformationByHandleEx(
                    handle,
                    FileIdBothDirectoryInfo,
                    buffer.as_mut_ptr().cast::<c_void>(),
                    buffer_bytes,
                )
            };
            if success == 0 {
                // End of enumeration (ERROR_NO_MORE_FILES) or access denied / similar.
                // Either way, stop: the preprocess is best-effort and correctness falls back
                // to per-file syscalls in `index_as_worktree`.
                break;
            }

            let mut offset = 0usize;
            loop {
                #[allow(clippy::cast_ptr_alignment)]
                #[allow(unsafe_code)]
                let info_ptr = unsafe { buffer.as_ptr().cast::<u8>().add(offset).cast::<FILE_ID_BOTH_DIR_INFO>() };
                #[allow(unsafe_code)]
                let info = unsafe { info_ptr.read_unaligned() };
                let info = &info;

                let name_len = (info.FileNameLength / 2) as usize;
                #[allow(unsafe_code)]
                let name_ptr = unsafe { (&raw const (*info_ptr).FileName).cast::<u16>() };
                #[allow(unsafe_code)]
                let name = unsafe { name_from_maybe_unaligned(name_ptr, name_len) };
                let name_slice: &[u16] = &name;

                let is_dot = name_len == 1 && name_slice[0] == b'.' as u16;
                let is_dotdot = name_len == 2 && name_slice[0] == b'.' as u16 && name_slice[1] == b'.' as u16;
                if !is_dot && !is_dotdot {
                    let mut filename = String::with_capacity(name_len);
                    let mut valid = true;
                    for ch in char::decode_utf16(name_slice.iter().copied()) {
                        match ch {
                            Ok(ch) => filename.push(ch),
                            Err(_) => {
                                valid = false;
                                break;
                            }
                        }
                    }
                    if valid {
                        entries.insert(filename.into_bytes().into(), stat_from_info(info));
                    }
                }

                if info.NextEntryOffset == 0 {
                    break;
                }
                offset += info.NextEntryOffset as usize;
            }
        }

        #[allow(unsafe_code)]
        unsafe {
            CloseHandle(handle)
        };
        Some(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_stat_to_index_stat() {
        let stat = Stat {
            is_dir: false,
            is_symlink: false,
            size: 1234,
            mtime_secs: 1700000000,
            mtime_nsecs: 500_000_000,
            ctime_secs: 1699999999,
            ctime_nsecs: 100_000_000,
        };
        let s = stat.to_stat();
        assert_eq!(s.size, 1234);
        assert_eq!(s.mtime.secs, 1700000000);
        assert_eq!(s.mtime.nsecs, 500_000_000);
        assert_eq!(s.ctime.secs, 1699999999);
        assert_eq!(s.ctime.nsecs, 100_000_000);
        // dev/ino/uid/gid are always zero on Windows — `Stat::from_fs` zeros them too.
        assert_eq!(s.dev, 0);
        assert_eq!(s.ino, 0);
        assert_eq!(s.uid, 0);
        assert_eq!(s.gid, 0);
    }

    #[test]
    fn type_flags_match_std_symlink_metadata_semantics() {
        use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT};

        let flags = super::windows::type_flags_from_attributes;
        const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
        const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;
        const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
        const IO_REPARSE_TAG_CLOUD: u32 = 0x9000_001A;
        const IO_REPARSE_TAG_WOF: u32 = 0x8000_0017;

        assert_eq!(flags(FILE_ATTRIBUTE_NORMAL, 0), (false, false), "plain file");
        assert_eq!(flags(FILE_ATTRIBUTE_DIRECTORY, 0), (true, false), "plain directory");
        assert_eq!(
            flags(FILE_ATTRIBUTE_REPARSE_POINT, IO_REPARSE_TAG_SYMLINK),
            (false, true),
            "file symlinks are symlinks, never directories, like std"
        );
        assert_eq!(
            flags(
                FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT,
                IO_REPARSE_TAG_SYMLINK
            ),
            (false, true),
            "directory symlinks are symlinks, never directories, like std"
        );
        assert_eq!(
            flags(
                FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT,
                IO_REPARSE_TAG_MOUNT_POINT
            ),
            (false, true),
            "junctions are treated as symlinks by std"
        );
        assert_eq!(
            flags(FILE_ATTRIBUTE_REPARSE_POINT, IO_REPARSE_TAG_CLOUD),
            (false, false),
            "non-surrogate reparse-point files are regular files"
        );
        assert_eq!(
            flags(
                FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT,
                IO_REPARSE_TAG_CLOUD
            ),
            (true, false),
            "non-surrogate reparse-point directories are regular directories"
        );
        assert_eq!(
            flags(FILE_ATTRIBUTE_REPARSE_POINT, IO_REPARSE_TAG_WOF),
            (false, false),
            "WOF reparse points are regular files"
        );
    }

    #[test]
    fn lazy_cache_populates_parent_directories_on_demand() {
        let temp_dir = unique_temp_dir();
        let worktree = temp_dir.path();

        std::fs::write(worktree.join("test.txt"), b"hello").expect("root file can be written");
        std::fs::create_dir(worktree.join("subdir")).expect("subdirectory can be created");
        std::fs::write(worktree.join("subdir").join("nested.txt"), b"world").expect("nested file can be written");

        let mut cache = FsCache::new(worktree);
        assert!(
            cache.get(b"test.txt".as_slice().into()).is_some(),
            "root files are found by lazily enumerating the worktree root"
        );
        assert!(
            cache.get(b"subdir/nested.txt".as_slice().into()).is_some(),
            "nested files are found by lazily enumerating their parent directory"
        );
        assert!(
            cache.get(b"Test.txt".as_slice().into()).is_none(),
            "mixed-case lookups miss rather than silently aliasing onto a different path"
        );
        assert!(
            cache.get(b"does-not-exist".as_slice().into()).is_none(),
            "missing files are not a problem (and cause a fall-through to the live path)"
        );
    }

    fn unique_temp_dir() -> gix_testtools::tempfile::TempDir {
        gix_testtools::tempfile::TempDir::new().expect("temporary directory can be created")
    }
}
