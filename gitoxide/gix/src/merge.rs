pub use gix_merge as plumbing;

pub use gix_merge::blob;

///
pub mod tree {
    use gix_merge::blob::builtin_driver;
    pub use gix_merge::tree::{Conflict, ContentMerge, Resolution, ResolutionFailure, UnresolvedConflict};

    /// The outcome produced by [`Repository::merge_trees()`](crate::Repository::merge_trees()).
    #[derive(Clone)]
    pub struct Outcome<'repo> {
        /// The ready-made (but unwritten) *base* tree, including all non-conflicting changes, and the changes that had
        /// conflicts which could be resolved automatically.
        ///
        /// This means, if all of their changes were conflicting, this will be equivalent to the *base* tree.
        pub tree: crate::object::tree::Editor<'repo>,
        /// The set of conflicts we encountered. Can be empty to indicate there was no conflict.
        /// Note that conflicts might have been auto-resolved, but they are listed here for completeness.
        /// Use [`has_unresolved_conflicts()`](Outcome::has_unresolved_conflicts()) to see if any action is needed
        /// before using [`tree`](Outcome::tree).
        pub conflicts: Vec<Conflict>,
        /// `true` if `conflicts` contains only a single *unresolved* conflict in the last slot, but possibly more resolved ones.
        /// This also makes this outcome a very partial merge that cannot be completed.
        pub failed_on_first_unresolved_conflict: bool,
    }

    impl Outcome<'_> {
        /// Return `true` if there is any conflict that would still need to be resolved as they would yield undesirable trees.
        /// This is based on `how` to determine what should be considered unresolved.
        pub fn has_unresolved_conflicts(&self, how: UnresolvedConflict) -> bool {
            self.conflicts.iter().any(|c| c.is_unresolved(how))
        }
    }

    /// A way to configure [`Repository::merge_trees()`](crate::Repository::merge_trees()).
    #[derive(Default, Debug, Clone)]
    pub struct Options {
        inner: gix_merge::tree::Options,
        file_favor: Option<FileFavor>,
    }

    impl From<gix_merge::tree::Options> for Options {
        fn from(opts: gix_merge::tree::Options) -> Self {
            Options {
                inner: opts,
                file_favor: None,
            }
        }
    }

    impl From<Options> for gix_merge::tree::Options {
        fn from(value: Options) -> Self {
            let mut opts = value.inner;
            if let Some(file_favor) = value.file_favor {
                let (resolve_binary, resolve_text) = match file_favor {
                    FileFavor::Ours => (
                        builtin_driver::binary::ResolveWith::Ours,
                        builtin_driver::text::Conflict::ResolveWithOurs,
                    ),
                    FileFavor::Theirs => (
                        builtin_driver::binary::ResolveWith::Theirs,
                        builtin_driver::text::Conflict::ResolveWithTheirs,
                    ),
                };

                opts.symlink_conflicts = Some(resolve_binary);
                opts.blob_merge.resolve_binary_with = Some(resolve_binary);
                opts.blob_merge.text.conflict = resolve_text;
            }
            opts
        }
    }

    /// Identify how files should be resolved in case of conflicts.
    ///
    /// This works for…
    ///
    /// * content merges
    /// * binary files
    /// * symlinks (a form of file after all)
    ///
    /// Note that that union merges aren't available as they aren't available for binaries or symlinks.
    #[derive(Debug, Copy, Clone)]
    pub enum FileFavor {
        /// Choose *our* side in case of a conflict.
        /// Note that this choice is precise, so *ours* hunk will only be chosen if they conflict with *theirs*,
        /// so *their* hunks may still show up in the merged result.
        Ours,
        /// Choose *their* side in case of a conflict.
        /// Note that this choice is precise, so *ours* hunk will only be chosen if they conflict with *theirs*,
        /// so *their* hunks may still show up in the merged result.
        Theirs,
    }

    /// Builder
    impl Options {
        /// If *not* `None`, rename tracking will be performed when determining the changes of each side of the merge.
        pub fn with_rewrites(mut self, rewrites: Option<gix_diff::Rewrites>) -> Self {
            self.inner.rewrites = rewrites;
            self
        }

        /// If `Some(what-is-unresolved)`, the first unresolved conflict will cause the entire merge to stop.
        /// This is useful to see if there is any conflict, without performing the whole operation, something
        /// that can be very relevant during merges that would cause a lot of blob-diffs.
        pub fn with_fail_on_conflict(mut self, fail_on_conflict: Option<UnresolvedConflict>) -> Self {
            self.inner.fail_on_conflict = fail_on_conflict;
            self
        }

        /// When `None`, the default, both sides will be treated equally, and in case of conflict an unbiased representation
        /// is chosen both for content and for trees, causing a conflict.
        /// When `Some(favor)` one can choose a side to prefer in order to automatically resolve a conflict meaningfully.
        pub fn with_file_favor(mut self, file_favor: Option<FileFavor>) -> Self {
            self.file_favor = file_favor;
            self
        }
    }
}
