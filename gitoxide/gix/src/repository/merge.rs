use crate::config::cache::util::ApplyLeniencyDefault;
use crate::config::tree;
use crate::repository::{blob_merge_options, merge_resource_cache, merge_trees, tree_merge_options};
use crate::Repository;
use gix_merge::blob::builtin_driver::text;
use gix_object::Write;
use std::borrow::Cow;

/// Merge-utilities
impl Repository {
    /// Create a resource cache that can hold the three resources needed for a three-way merge. `worktree_roots`
    /// determines which side of the merge is read from the worktree, or from which worktree.
    ///
    /// The platform can be used to set up resources and finally perform a merge among blobs.
    ///
    /// Note that the current index is used for attribute queries.
    pub fn merge_resource_cache(
        &self,
        worktree_roots: gix_merge::blob::pipeline::WorktreeRoots,
    ) -> Result<gix_merge::blob::Platform, merge_resource_cache::Error> {
        let index = self.index_or_load_from_head()?;
        let mode = {
            let renormalize = self
                .config
                .resolved
                .boolean(&tree::Merge::RENORMALIZE)
                .map(|res| {
                    tree::Merge::RENORMALIZE
                        .enrich_error(res)
                        .with_lenient_default(self.config.lenient_config)
                })
                .transpose()?
                .unwrap_or_default();
            if renormalize {
                gix_merge::blob::pipeline::Mode::Renormalize
            } else {
                gix_merge::blob::pipeline::Mode::ToGit
            }
        };
        let attrs = self
            .attributes_only(
                &index,
                if worktree_roots.is_unset() {
                    gix_worktree::stack::state::attributes::Source::IdMapping
                } else {
                    gix_worktree::stack::state::attributes::Source::WorktreeThenIdMapping
                },
            )?
            .inner;
        let filter = gix_filter::Pipeline::new(self.command_context()?, crate::filter::Pipeline::options(self)?);
        let filter = gix_merge::blob::Pipeline::new(worktree_roots, filter, self.config.merge_pipeline_options()?);
        let options = gix_merge::blob::platform::Options {
            default_driver: self.config.resolved.string(&tree::Merge::DEFAULT).map(Cow::into_owned),
        };
        let drivers = self.config.merge_drivers()?;
        Ok(gix_merge::blob::Platform::new(filter, mode, attrs, drivers, options))
    }

    /// Return options for use with [`gix_merge::blob::PlatformRef::merge()`], accessible through
    /// [merge_resource_cache()](Self::merge_resource_cache).
    pub fn blob_merge_options(&self) -> Result<gix_merge::blob::platform::merge::Options, blob_merge_options::Error> {
        Ok(gix_merge::blob::platform::merge::Options {
            is_virtual_ancestor: false,
            resolve_binary_with: None,
            text: gix_merge::blob::builtin_driver::text::Options {
                diff_algorithm: self.diff_algorithm()?,
                conflict: text::Conflict::Keep {
                    style: self
                        .config
                        .resolved
                        .string(&tree::Merge::CONFLICT_STYLE)
                        .map(|value| {
                            tree::Merge::CONFLICT_STYLE
                                .try_into_conflict_style(value)
                                .with_lenient_default(self.config.lenient_config)
                        })
                        .transpose()?
                        .unwrap_or_default(),
                    marker_size: text::Conflict::DEFAULT_MARKER_SIZE.try_into().unwrap(),
                },
            },
        })
    }

    /// Read all relevant configuration options to instantiate options for use in [`merge_trees()`](Self::merge_trees).
    pub fn tree_merge_options(&self) -> Result<gix_merge::tree::Options, tree_merge_options::Error> {
        Ok(gix_merge::tree::Options {
            rewrites: crate::diff::utils::new_rewrites_inner(
                &self.config.resolved,
                self.config.lenient_config,
                &tree::Merge::RENAMES,
                &tree::Merge::RENAME_LIMIT,
            )?,
            blob_merge: self.blob_merge_options()?,
            blob_merge_command_ctx: self.command_context()?,
            fail_on_conflict: None,
            marker_size_multiplier: 0,
            symlink_conflicts: None,
            allow_lossy_resolution: false,
        })
    }

    /// Merge `our_tree` and `their_tree` together, assuming they have the same `ancestor_tree`, to yield a new tree
    /// which is provided as [tree editor](gix_object::tree::Editor) to inspect and finalize results at will.
    /// No change to the worktree or index is made, but objects may be written to the object database as merge results
    /// are stored.
    /// If these changes should not be observable outside of this instance, consider [enabling object memory](Self::with_object_memory).
    ///
    /// Note that `ancestor_tree` can be the [empty tree hash](gix_hash::ObjectId::empty_tree) to indicate no common ancestry.
    ///
    /// `labels` are typically chosen to identify the refs or names for `our_tree` and `their_tree` and `ancestor_tree` respectively.
    ///
    /// `options` should be initialized with [`tree_merge_options()`](Self::tree_merge_options()).
    // TODO: Use `crate::merge::Options` here and add niceties such as setting the resolution strategy.
    pub fn merge_trees(
        &self,
        ancestor_tree: impl AsRef<gix_hash::oid>,
        our_tree: impl AsRef<gix_hash::oid>,
        their_tree: impl AsRef<gix_hash::oid>,
        labels: gix_merge::blob::builtin_driver::text::Labels<'_>,
        options: gix_merge::tree::Options,
    ) -> Result<gix_merge::tree::Outcome<'_>, merge_trees::Error> {
        let mut diff_cache = self.diff_resource_cache_for_tree_diff()?;
        let mut blob_merge = self.merge_resource_cache(Default::default())?;
        Ok(gix_merge::tree(
            ancestor_tree.as_ref(),
            our_tree.as_ref(),
            their_tree.as_ref(),
            labels,
            self,
            |buf| self.write_buf(gix_object::Kind::Blob, buf),
            &mut Default::default(),
            &mut diff_cache,
            &mut blob_merge,
            options,
        )?)
    }
}
