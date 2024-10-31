/// The error returned by [`commit()`](crate::commit()).
#[derive(Debug, thiserror::Error)]
#[allow(missing_docs)]
pub enum Error {
    #[error(transparent)]
    MergeBase(#[from] gix_revision::merge_base::Error),
    #[error(transparent)]
    MergeTree(#[from] crate::tree::Error),
    #[error("Failed to write tree for merged merge-base or virtual commit")]
    WriteObject(gix_object::write::Error),
    #[error("No common ancestor between {our_commit_id} and {their_commit_id}")]
    NoMergeBase {
        /// The commit on our side that was to be merged.
        our_commit_id: gix_hash::ObjectId,
        /// The commit on their side that was to be merged.
        their_commit_id: gix_hash::ObjectId,
    },
    #[error(
        "Conflicts occurred when trying to resolve multiple merge-bases by merging them. This is most certainly a bug."
    )]
    VirtualMergeBaseConflict,
    #[error("Could not find ancestor, our or their commit to extract tree from")]
    FindCommit(#[from] gix_object::find::existing_object::Error),
}

/// A way to configure [`commit()`](crate::commit()).
#[derive(Default, Debug, Clone)]
pub struct Options {
    /// If `true`, merging unrelated commits is allowed, with the merge-base being assumed as empty tree.
    pub allow_missing_merge_base: bool,
    /// Options to define how trees should be merged.
    pub tree_merge: crate::tree::Options,
    /// If `true`, do not merge multiple merge-bases into one. Instead, just use the first one.
    // TODO: test
    #[doc(alias = "no_recursive", alias = "git2")]
    pub use_first_merge_base: bool,
}

/// The result of [`commit()`](crate::commit()).
#[derive(Clone)]
pub struct Outcome<'a> {
    /// The outcome of the actual tree-merge.
    pub tree_merge: crate::tree::Outcome<'a>,
    /// The tree id of the base commit we used. This is either…
    /// * the single merge-base we found
    /// * the first of multiple merge-bases if [`use_first_merge_base`](Options::use_first_merge_base) was `true`.
    /// * the merged tree of all merge-bases, which then isn't linked to an actual commit.
    /// * an empty tree, if [`allow_missing_merge_base`](Options::allow_missing_merge_base) is enabled.
    pub merge_base_tree_id: gix_hash::ObjectId,
    /// The object ids of all the commits which were found to be merge-bases, or `None` if there was no merge-base.
    pub merge_bases: Option<Vec<gix_hash::ObjectId>>,
    /// A list of virtual commits that were created to merge multiple merge-bases into one.
    /// As they are not reachable by anything they will be garbage collected, but knowing them provides options.
    pub virtual_merge_bases: Vec<gix_hash::ObjectId>,
}

pub(super) mod function {
    use crate::blob::builtin_driver;
    use crate::commit::{Error, Options};
    use crate::tree::UnresolvedConflict;
    use gix_object::FindExt;
    use std::borrow::Cow;

    /// Like [`tree()`](crate::tree()), but it takes only two commits, `our_commit` and `their_commit` to automatically
    /// compute the merge-bases among them.
    /// If there are multiple merge bases, these will be auto-merged into one, recursively, if
    /// [`allow_missing_merge_base`](Options::allow_missing_merge_base) is `true`.
    ///
    /// `labels` are names where [`current`](crate::blob::builtin_driver::text::Labels::current) is a name for `our_commit`
    /// and [`other`](crate::blob::builtin_driver::text::Labels::other) is a name for `their_commit`.
    /// If [`ancestor`](crate::blob::builtin_driver::text::Labels::ancestor) is unset, it will be set by us based on the
    /// merge-bases of `our_commit` and `their_commit`.
    ///
    /// The `graph` is used to find the merge-base between `our_commit` and `their_commit`, and can also act as cache
    /// to speed up subsequent merge-base queries.
    ///
    /// Use `abbreviate_hash(id)` to shorten the given `id` according to standard git shortening rules. It's used in case
    /// the ancestor-label isn't explicitly set so that the merge base label becomes the shortened `id`.
    /// Note that it's a dyn closure only to make it possible to recursively call this function in case of multiple merge-bases.
    ///
    /// `write_object` is used only if it's allowed to merge multiple merge-bases into one, and if there
    /// are multiple merge bases, and to write merged buffers as blobs.
    ///
    /// ### Performance
    ///
    /// Note that `objects` *should* have an object cache to greatly accelerate tree-retrieval.
    ///
    /// ### Notes
    ///
    /// When merging merge-bases recursively, the options are adjusted automatically to act like Git, i.e. merge binary
    /// blobs and resolve with *ours*, while resorting to using the base/ancestor in case of unresolvable conflicts.
    ///
    /// ### Deviation
    ///
    /// * It's known that certain conflicts around symbolic links can be auto-resolved. We don't have an option for this
    ///   at all, yet, primarily as Git seems to not implement the *ours*/*theirs* choice in other places even though it
    ///   reasonably could. So we leave it to the caller to continue processing the returned tree at will.
    #[allow(clippy::too_many_arguments)]
    pub fn commit<'objects>(
        our_commit: gix_hash::ObjectId,
        their_commit: gix_hash::ObjectId,
        labels: builtin_driver::text::Labels<'_>,
        graph: &mut gix_revwalk::Graph<'_, '_, gix_revwalk::graph::Commit<gix_revision::merge_base::Flags>>,
        diff_resource_cache: &mut gix_diff::blob::Platform,
        blob_merge: &mut crate::blob::Platform,
        objects: &'objects (impl gix_object::FindObjectOrHeader + gix_object::Write),
        abbreviate_hash: &mut dyn FnMut(&gix_hash::oid) -> String,
        options: Options,
    ) -> Result<super::Outcome<'objects>, Error> {
        let merge_bases = gix_revision::merge_base(our_commit, &[their_commit], graph)?;
        let mut virtual_merge_bases = Vec::new();
        let mut state = gix_diff::tree::State::default();
        let mut commit_to_tree =
            |commit_id: gix_hash::ObjectId| objects.find_commit(&commit_id, &mut state.buf1).map(|c| c.tree());

        let (merge_base_tree_id, ancestor_name): (_, Cow<'_, str>) = match merge_bases.clone() {
            Some(base_commit) if base_commit.len() == 1 => {
                (commit_to_tree(base_commit[0])?, abbreviate_hash(&base_commit[0]).into())
            }
            Some(mut base_commits) => {
                let virtual_base_tree = if options.use_first_merge_base {
                    let first = *base_commits.first().expect("if Some() there is at least one.");
                    commit_to_tree(first)?
                } else {
                    let mut merged_commit_id = base_commits.pop().expect("at least one base");
                    let mut options = options.clone();
                    options.tree_merge.allow_lossy_resolution = true;
                    options.tree_merge.blob_merge.is_virtual_ancestor = true;
                    options.tree_merge.blob_merge.text.conflict = builtin_driver::text::Conflict::ResolveWithOurs;
                    let favor_ancestor = Some(builtin_driver::binary::ResolveWith::Ancestor);
                    options.tree_merge.blob_merge.resolve_binary_with = favor_ancestor;
                    options.tree_merge.symlink_conflicts = favor_ancestor;
                    let labels = builtin_driver::text::Labels {
                        current: Some("Temporary merge branch 1".into()),
                        other: Some("Temporary merge branch 2".into()),
                        ..labels
                    };
                    while let Some(next_commit_id) = base_commits.pop() {
                        options.tree_merge.marker_size_multiplier += 1;
                        let mut out = commit(
                            merged_commit_id,
                            next_commit_id,
                            labels,
                            graph,
                            diff_resource_cache,
                            blob_merge,
                            objects,
                            abbreviate_hash,
                            options.clone(),
                        )?;
                        // This shouldn't happen, but if for some buggy reason it does, we rather bail.
                        if out
                            .tree_merge
                            .has_unresolved_conflicts(UnresolvedConflict::ConflictMarkers)
                        {
                            return Err(Error::VirtualMergeBaseConflict);
                        }
                        let merged_tree_id = out
                            .tree_merge
                            .tree
                            .write(|tree| objects.write(tree))
                            .map_err(Error::WriteObject)?;

                        merged_commit_id =
                            create_virtual_commit(objects, merged_commit_id, next_commit_id, merged_tree_id)?;

                        virtual_merge_bases.extend(out.virtual_merge_bases);
                        virtual_merge_bases.push(merged_commit_id);
                    }
                    commit_to_tree(merged_commit_id)?
                };
                (virtual_base_tree, "merged common ancestors".into())
            }
            None => {
                if options.allow_missing_merge_base {
                    (gix_hash::ObjectId::empty_tree(our_commit.kind()), "empty tree".into())
                } else {
                    return Err(Error::NoMergeBase {
                        our_commit_id: our_commit,
                        their_commit_id: their_commit,
                    });
                }
            }
        };

        let mut labels = labels; // TODO(borrowchk): this re-assignment shouldn't be needed.
        if labels.ancestor.is_none() {
            labels.ancestor = Some(ancestor_name.as_ref().into());
        }

        let our_tree_id = objects.find_commit(&our_commit, &mut state.buf1)?.tree();
        let their_tree_id = objects.find_commit(&their_commit, &mut state.buf1)?.tree();

        let outcome = crate::tree(
            &merge_base_tree_id,
            &our_tree_id,
            &their_tree_id,
            labels,
            objects,
            |buf| objects.write_buf(gix_object::Kind::Blob, buf),
            &mut state,
            diff_resource_cache,
            blob_merge,
            options.tree_merge,
        )?;

        Ok(super::Outcome {
            tree_merge: outcome,
            merge_bases,
            merge_base_tree_id,
            virtual_merge_bases,
        })
    }

    fn create_virtual_commit(
        objects: &(impl gix_object::Find + gix_object::Write),
        parent_a: gix_hash::ObjectId,
        parent_b: gix_hash::ObjectId,
        tree_id: gix_hash::ObjectId,
    ) -> Result<gix_hash::ObjectId, Error> {
        let mut buf = Vec::new();
        let mut commit: gix_object::Commit = objects.find_commit(&parent_a, &mut buf)?.into();
        commit.parents = vec![parent_a, parent_b].into();
        commit.tree = tree_id;
        objects.write(&commit).map_err(Error::WriteObject)
    }
}
