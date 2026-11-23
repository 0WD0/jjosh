use crate::{File, Version, write};
use gix_error::Result;
use gix_fs::SharedPermissions;

impl File {
    /// Write the index to `out` with `options`, to be readable by [`File::at()`], returning the version that was actually written
    /// to retain all information of this index.
    /// Also returns the checksum written to the trailer, which is null if [`write::Options::skip_hash`] is set.
    /// Unlike [`File::checksum()`], this preserves the on-disk representation of an omitted checksum.
    ///
    /// Cache extensions are invalidated by default, without mutating the in-memory caches.
    /// See [`File::write()`] for how to retain valid caches after changing entries.
    pub fn write_to(
        &self,
        mut out: impl std::io::Write,
        options: write::Options,
    ) -> Result<(Version, gix_hash::ObjectId)> {
        let _span = gix_trace::detail!("gix_index::File::write_to()", skip_hash = options.skip_hash);
        let (version, hash) = if options.skip_hash {
            let out: &mut dyn std::io::Write = &mut out;
            let version = self.state.write_to(out, options)?;
            (version, self.state.object_hash.null())
        } else {
            let mut hasher = gix_hash::io::Write::new(&mut out, self.state.object_hash);
            let out: &mut dyn std::io::Write = &mut hasher;
            let version = self.state.write_to(out, options)?;
            (version, hasher.hash.try_finalize()?)
        };
        out.write_all(hash.as_slice()).map_err(gix_hash::io::from_std_io)?;
        Ok((version, hash))
    }

    /// Write ourselves to the path we were read from after acquiring a lock, using `options`.
    ///
    /// The checksum is updated after a successful write, or cleared if [`write::Options::skip_hash`] is set.
    /// The sharing policy `shared_repository_permissions` applies to the newly written index.
    ///
    /// ### Cache extensions
    ///
    /// By default, [`write::Extensions::All`] writes all supported optional extensions, but conservatively invalidates
    /// the tree-cache and untracked cache so Git recomputes them. The in-memory caches are left untouched.
    ///
    /// [`write::Extensions::AllUnconditionally`] and [`write::Extensions::Given`] write selected caches
    /// as-is. They must already match the current entries and worktree: a stale tree-cache can cause
    /// `git commit` or `git write-tree` to capture outdated subtree content, and a stale untracked cache
    /// can hide files whose tracked status changed.
    ///
    /// A newly constructed index can use [`State::copy_metadata_from()`](crate::State::copy_metadata_from())
    /// with its original state to retain and selectively invalidate both caches before writing with
    /// [`write::Extensions::AllUnconditionally`].
    pub fn write(&mut self, options: write::Options, shared_repository_permissions: SharedPermissions) -> Result {
        self.write_with(options, shared_repository_permissions, |_| Ok(()))
    }

    /// Write ourselves like [`File::write()`], invoking `before_commit` after all serialized bytes have been flushed
    /// to the index lock file, but before committing it to the index path.
    ///
    /// The callback receives the held lock so it can inspect the staged bytes at [`gix_lock::File::lock_path()`]
    /// and the original index at [`gix_lock::File::resource_path()`]. If it fails, the lock is dropped without
    /// changing the original index or updating our version and checksum.
    ///
    /// The cache considerations documented on [`File::write()`] also apply here.
    pub fn write_with(
        &mut self,
        options: write::Options,
        shared_repository_permissions: SharedPermissions,
        before_commit: impl FnOnce(&gix_lock::File) -> Result,
    ) -> Result {
        use gix_error::{ResultExt, message};

        let _span = gix_trace::detail!("gix_index::File::write()", path = ?self.path);
        let mut lock = std::io::BufWriter::with_capacity(
            64 * 1024,
            gix_lock::File::acquire(
                &self.path,
                gix_lock::acquire::Fail::Immediately,
                None,
                shared_repository_permissions,
                None,
            )
            .or_raise(|| message("Could not acquire lock for index file"))?,
        );
        let (version, digest) = self
            .write_to(&mut lock, options)
            .or_raise(|| message("Could not write index"))?;
        let lock = lock
            .into_inner()
            .map_err(|err| err.into_error())
            .or_raise(|| message("Could not flush buffered index data"))?;
        before_commit(&lock).or_raise(|| message("Index write rejected before committing the lock"))?;
        lock.commit()
            .or_raise(|| message("Could not commit lock for index file"))?;
        self.state.version = version;
        self.checksum = (!digest.is_null()).then_some(digest);
        Ok(())
    }
}
