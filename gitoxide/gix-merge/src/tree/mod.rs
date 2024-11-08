use bstr::BString;
use gix_diff::tree_with_rewrites::Change;
use gix_diff::Rewrites;

/// The error returned by [`tree()`](crate::tree()).
#[derive(Debug, thiserror::Error)]
#[allow(missing_docs)]
pub enum Error {
    #[error("Could not find ancestor, our or their tree to get started")]
    FindTree(#[from] gix_object::find::existing_object::Error),
    #[error("Could not find ancestor, our or their tree iterator to get started")]
    FindTreeIter(#[from] gix_object::find::existing_iter::Error),
    #[error("Failed to diff our side or their side")]
    DiffTree(#[from] gix_diff::tree_with_rewrites::Error),
    #[error("Could not apply merge result to base tree")]
    TreeEdit(#[from] gix_object::tree::editor::Error),
    #[error("Failed to load resource to prepare for blob merge")]
    BlobMergeSetResource(#[from] crate::blob::platform::set_resource::Error),
    #[error(transparent)]
    BlobMergePrepare(#[from] crate::blob::platform::prepare_merge::Error),
    #[error(transparent)]
    BlobMerge(#[from] crate::blob::platform::merge::Error),
    #[error("Failed to write merged blob content as blob to the object database")]
    WriteBlobToOdb(Box<dyn std::error::Error + Send + Sync + 'static>),
    #[error("The merge was performed, but the binary merge result couldn't be selected as it wasn't found")]
    MergeResourceNotFound,
}

/// The outcome produced by [`tree()`](crate::tree()).
#[derive(Clone)]
pub struct Outcome<'a> {
    /// The ready-made (but unwritten) *base* tree, including all non-conflicting changes, and the changes that had
    /// conflicts which could be resolved automatically.
    ///
    /// This means, if all of their changes were conflicting, this will be equivalent to the *base* tree.
    pub tree: gix_object::tree::Editor<'a>,
    /// The set of conflicts we encountered. Can be empty to indicate there was no conflict.
    /// Note that conflicts might have been auto-resolved, but they are listed here for completeness.
    /// Use [`has_unresolved_conflicts()`](Outcome::has_unresolved_conflicts()) to see if any action is needed
    /// before using [`tree`](Outcome::tree).
    pub conflicts: Vec<Conflict>,
    /// `true` if `conflicts` contains only a single *unresolved* conflict in the last slot, but possibly more resolved ones.
    /// This also makes this outcome a very partial merge that cannot be completed.
    /// Only set if [`fail_on_conflict`](Options::fail_on_conflict) is `true`.
    pub failed_on_first_unresolved_conflict: bool,
}

/// Determine what should be considered an unresolved conflict.
///
/// Note that no matter which variant, [conflicts](Conflict) with
/// [resolution failure](`ResolutionFailure`) will always be unresolved.
///
/// Also, when one side was modified but the other side renamed it, this will not
/// be considered a conflict, even if a non-conflicting merge happened.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum TreatAsUnresolved {
    /// Only consider content merges with conflict markers as unresolved.
    ///
    /// Auto-resolved tree conflicts will *not* be considered unresolved.
    ConflictMarkers,
    /// Consider content merges with conflict markers as unresolved, and content
    /// merges where conflicts where auto-resolved in any way, like choosing
    /// *ours*, *theirs*  or by their *union*.
    ///
    /// Auto-resolved tree conflicts will *not* be considered unresolved.
    ConflictMarkersAndAutoResolved,
    /// Whenever there were conflicting renames, or conflict markers, it is unresolved.
    /// Note that auto-resolved content merges will *not* be considered unresolved.
    ///
    /// Also note that files that were changed in one and renamed in another will
    /// be moved into place, which will be considered resolved.
    Renames,
    /// Similar to [`Self::Renames`], but auto-resolved content-merges will
    /// also be considered unresolved.
    RenamesAndAutoResolvedContent,
}

impl Outcome<'_> {
    /// Return `true` if there is any conflict that would still need to be resolved as they would yield undesirable trees.
    /// This is based on `how` to determine what should be considered unresolved.
    pub fn has_unresolved_conflicts(&self, how: TreatAsUnresolved) -> bool {
        self.conflicts.iter().any(|c| c.is_unresolved(how))
    }
}

/// A description of a conflict (i.e. merge issue without an auto-resolution) as seen during a [tree-merge](crate::tree()).
/// They may have a resolution that was applied automatically, or be left for the caller to resolved.
#[derive(Debug, Clone)]
pub struct Conflict {
    /// A record on how the conflict resolution succeeded with `Ok(_)` or failed with `Err(_)`.
    /// Note that in case of `Err(_)`, edits may still have been made to the tree to aid resolution.
    /// On failure, one can examine `ours` and `theirs` to potentially find a custom solution.
    /// Note that the descriptions of resolutions or resolution failures may be swapped compared
    /// to the actual changes. This is due to changes like `modification|deletion` being treated the
    /// same as `deletion|modification`, i.e. *ours* is not more privileged than theirs.
    /// To compensate for that, use [`changes_in_resolution()`](Conflict::changes_in_resolution()).
    pub resolution: Result<Resolution, ResolutionFailure>,
    /// The change representing *our* side.
    pub ours: Change,
    /// The change representing *their* side.
    pub theirs: Change,
    /// Determine how to interpret the `ours` and `theirs` fields. This is used to implement [`Self::changes_in_resolution()`]
    /// and [`Self::into_parts_by_resolution()`].
    map: ConflictMapping,
}

/// A utility to help define which side is what in the [`Conflict`] type.
#[derive(Debug, Clone, Copy)]
enum ConflictMapping {
    /// The sides are as described in the field documentation, i.e. `ours` is `ours`.
    Original,
    /// The sides are the opposite of the field documentation. i.e. `ours` is `theirs` and `theirs` is `ours`.
    Swapped,
}

impl ConflictMapping {
    fn is_swapped(&self) -> bool {
        matches!(self, ConflictMapping::Swapped)
    }
    fn swapped(self) -> ConflictMapping {
        match self {
            ConflictMapping::Original => ConflictMapping::Swapped,
            ConflictMapping::Swapped => ConflictMapping::Original,
        }
    }
}

impl Conflict {
    /// Return `true` if this instance is considered unresolved based on the criterion specified by `how`.
    pub fn is_unresolved(&self, how: TreatAsUnresolved) -> bool {
        use crate::blob;
        let content_merge_matches = |info: &ContentMerge| match how {
            TreatAsUnresolved::ConflictMarkers | TreatAsUnresolved::Renames => {
                matches!(info.resolution, blob::Resolution::Conflict)
            }
            TreatAsUnresolved::RenamesAndAutoResolvedContent | TreatAsUnresolved::ConflictMarkersAndAutoResolved => {
                matches!(
                    info.resolution,
                    blob::Resolution::Conflict | blob::Resolution::CompleteWithAutoResolvedConflict
                )
            }
        };
        match how {
            TreatAsUnresolved::ConflictMarkers | TreatAsUnresolved::ConflictMarkersAndAutoResolved => {
                self.resolution.is_err() || self.content_merge().map_or(false, |info| content_merge_matches(&info))
            }
            TreatAsUnresolved::Renames | TreatAsUnresolved::RenamesAndAutoResolvedContent => match &self.resolution {
                Ok(success) => match success {
                    Resolution::SourceLocationAffectedByRename { .. } => false,
                    Resolution::OursModifiedTheirsRenamedAndChangedThenRename { .. } => true,
                    Resolution::OursModifiedTheirsModifiedThenBlobContentMerge { merged_blob } => {
                        content_merge_matches(merged_blob)
                    }
                },
                Err(_failure) => true,
            },
        }
    }

    /// Returns the changes of fields `ours` and `theirs` so they match their description in the
    /// [`Resolution`] or [`ResolutionFailure`] respectively.
    /// Without this, the sides may appear swapped as `ours|theirs` is treated the same as `theirs/ours`
    /// if both types are different, like `modification|deletion`.
    pub fn changes_in_resolution(&self) -> (&Change, &Change) {
        match self.map {
            ConflictMapping::Original => (&self.ours, &self.theirs),
            ConflictMapping::Swapped => (&self.theirs, &self.ours),
        }
    }

    /// Similar to [`changes_in_resolution()`](Self::changes_in_resolution()), but returns the parts
    /// of the structure so the caller can take ownership. This can be useful when applying your own
    /// resolutions for resolution failures.
    pub fn into_parts_by_resolution(self) -> (Result<Resolution, ResolutionFailure>, Change, Change) {
        match self.map {
            ConflictMapping::Original => (self.resolution, self.ours, self.theirs),
            ConflictMapping::Swapped => (self.resolution, self.theirs, self.ours),
        }
    }

    /// Return information about the content merge if it was performed.
    pub fn content_merge(&self) -> Option<ContentMerge> {
        match &self.resolution {
            Ok(success) => match success {
                Resolution::SourceLocationAffectedByRename { .. } => None,
                Resolution::OursModifiedTheirsRenamedAndChangedThenRename { merged_blob, .. } => *merged_blob,
                Resolution::OursModifiedTheirsModifiedThenBlobContentMerge { merged_blob } => Some(*merged_blob),
            },
            Err(failure) => match failure {
                ResolutionFailure::OursRenamedTheirsRenamedDifferently { merged_blob } => *merged_blob,
                ResolutionFailure::Unknown
                | ResolutionFailure::OursModifiedTheirsDeleted
                | ResolutionFailure::OursModifiedTheirsRenamedTypeMismatch
                | ResolutionFailure::OursModifiedTheirsDirectoryThenOursRenamed {
                    renamed_unique_path_to_modified_blob: _,
                }
                | ResolutionFailure::OursAddedTheirsAddedTypeMismatch { .. }
                | ResolutionFailure::OursDeletedTheirsRenamed => None,
            },
        }
    }
}

/// Describes of a conflict involving *our* change and *their* change was specifically resolved.
///
/// Note that all resolutions are side-agnostic, so *ours* could also have been *theirs* and vice versa.
/// Also note that symlink merges are always done via binary merge, using the same logic.
#[derive(Debug, Clone)]
pub enum Resolution {
    /// *ours* had a renamed directory and *theirs* made a change in the now renamed directory.
    /// We moved that change into its location.
    SourceLocationAffectedByRename {
        /// The repository-relative path to the location that the change ended up in after
        /// being affected by a renamed directory.
        final_location: BString,
    },
    /// *ours* was a modified blob and *theirs* renamed that blob.
    /// We moved the changed blob from *ours* to its new location, and merged it successfully.
    /// If this is a `copy`, the source of the copy was set to be the changed blob as well so both match.
    OursModifiedTheirsRenamedAndChangedThenRename {
        /// If one side added the executable bit, we always add it in the merged result.
        merged_mode: Option<gix_object::tree::EntryMode>,
        /// If `Some(…)`, the content of the involved blob had to be merged.
        merged_blob: Option<ContentMerge>,
        /// The repository relative path to the location the blob finally ended up in.
        /// It's `Some()` only if *they* rewrote the blob into a directory which *we* renamed on *our* side.
        final_location: Option<BString>,
    },
    /// *ours* and *theirs* carried changes and where content-merged.
    ///
    /// Note that *ours* and *theirs* may also be rewrites with the same destination and mode,
    /// or additions.
    OursModifiedTheirsModifiedThenBlobContentMerge {
        /// The outcome of the content merge.
        merged_blob: ContentMerge,
    },
}

/// Describes of a conflict involving *our* change and *their* failed to be resolved.
#[derive(Debug, Clone)]
pub enum ResolutionFailure {
    /// *ours* was renamed, but *theirs* was renamed differently. Both versions will be present in the tree,
    OursRenamedTheirsRenamedDifferently {
        /// If `Some(…)`, the content of the involved blob had to be merged.
        merged_blob: Option<ContentMerge>,
    },
    /// *ours* was modified, but *theirs* was turned into a directory, so *ours* was renamed to a non-conflicting path.
    OursModifiedTheirsDirectoryThenOursRenamed {
        /// The path at which `ours` can be found in the tree - it's in the same directory that it was in before.
        renamed_unique_path_to_modified_blob: BString,
    },
    /// *ours* was added (or renamed into place) with a different mode than theirs, e.g. blob and symlink, and we kept
    /// the symlink in its original location, renaming the other side to `their_unique_location`.
    OursAddedTheirsAddedTypeMismatch {
        /// The location at which *their* state was placed to resolve the name and type clash, named to indicate
        /// where the entry is coming from.
        their_unique_location: BString,
    },
    /// *ours* was modified, and they renamed the same file, but there is also a non-mergable type-change.
    /// Here we keep both versions of the file.
    OursModifiedTheirsRenamedTypeMismatch,
    /// *ours* was deleted, but *theirs* was renamed.
    OursDeletedTheirsRenamed,
    /// *ours* was modified and *theirs* was deleted. We keep the modified one and ignore the deletion.
    OursModifiedTheirsDeleted,
    /// *ours* and *theirs* are in an untested state so it can't be handled yet, and is considered a conflict
    /// without adding our *or* their side to the resulting tree.
    Unknown,
}

/// Information about a blob content merge for use in a [`Resolution`].
/// Note that content merges always count as success to avoid duplication of cases, which forces callers
/// to check for the [`resolution`](Self::resolution) field.
#[derive(Debug, Copy, Clone)]
pub struct ContentMerge {
    /// The fully merged blob.
    pub merged_blob_id: gix_hash::ObjectId,
    /// Identify the kind of resolution of the blob merge. Note that it may be conflicting.
    pub resolution: crate::blob::Resolution,
}

/// A way to configure [`tree()`](crate::tree()).
#[derive(Default, Debug, Clone)]
pub struct Options {
    /// If *not* `None`, rename tracking will be performed when determining the changes of each side of the merge.
    pub rewrites: Option<Rewrites>,
    /// Decide how blob-merges should be done. This relates to if conflicts can be resolved or not.
    pub blob_merge: crate::blob::platform::merge::Options,
    /// The context to use when invoking merge-drivers.
    pub blob_merge_command_ctx: gix_command::Context,
    /// If `Some(what-is-unresolved)`, the first unresolved conflict will cause the entire merge to stop.
    /// This is useful to see if there is any conflict, without performing the whole operation, something
    /// that can be very relevant during merges that would cause a lot of blob-diffs.
    pub fail_on_conflict: Option<TreatAsUnresolved>,
    /// This value also affects the size of merge-conflict markers, to allow differentiating
    /// merge conflicts on each level, for any value greater than 0, with values `N` causing `N*2`
    /// markers to be added to the configured value.
    ///
    /// This is used automatically when merging merge-bases recursively.
    pub marker_size_multiplier: u8,
    /// If `None`, when symlinks clash *ours* will be chosen and a conflict will occur.
    /// Otherwise, the same logic applies as for the merge of binary resources.
    pub symlink_conflicts: Option<crate::blob::builtin_driver::binary::ResolveWith>,
    /// If `true`, instead of issuing a conflict with [`ResolutionFailure`], do nothing and keep the base/ancestor
    /// version. This is useful when one wants to avoid any kind of merge-conflict to have *some*, *lossy* resolution.
    pub allow_lossy_resolution: bool,
}

pub(super) mod function;
mod utils;
