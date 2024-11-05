#[cfg(feature = "blob-merge")]
pub use gix_merge as plumbing;

pub use gix_merge::blob;

///
pub mod tree {
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
}
