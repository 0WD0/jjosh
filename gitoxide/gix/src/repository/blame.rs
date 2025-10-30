use gix_hash::ObjectId;
use gix_ref::bstr::BStr;

use crate::{repository::blame_file, Repository};

/// Options to be passed to [Repository::blame_file()](crate::Repository::blame_file()).
#[derive(Default, Debug, Clone)]
pub struct Options {
    /// The algorithm to use for diffing. If this is `None`, `diff.algorithm` will be used.
    pub diff_algorithm: Option<gix_diff::blob::Algorithm>,
    /// The ranges to blame in the file.
    pub ranges: gix_blame::BlameRanges,
    /// Don't consider commits before the given date.
    pub since: Option<gix_date::Time>,
    /// Determine if rename tracking should be performed, and how.
    pub rewrites: Option<gix_diff::Rewrites>,
}

impl Repository {
    /// Produce a list of consecutive [`gix_blame::BlameEntry`] instances. Each `BlameEntry`
    /// corresponds to a hunk of consecutive lines of the file at `suspect:<file_path>` that got
    /// introduced by a specific commit.
    ///
    /// For details, see the documentation of [`gix_blame::file()`].
    pub fn blame_file(
        &self,
        file_path: &BStr,
        suspect: impl Into<ObjectId>,
        options: Options,
    ) -> Result<gix_blame::Outcome, blame_file::Error> {
        let cache: Option<gix_commitgraph::Graph> = self.commit_graph_if_enabled()?;
        let mut resource_cache = self.diff_resource_cache_for_tree_diff()?;

        let Options {
            diff_algorithm,
            ranges,
            since,
            rewrites,
        } = options;
        let diff_algorithm = match diff_algorithm {
            Some(diff_algorithm) => diff_algorithm,
            None => self.diff_algorithm()?,
        };

        let options = gix_blame::Options {
            diff_algorithm,
            ranges,
            since,
            rewrites,
            debug_track_path: false,
        };

        let outcome = gix_blame::file(
            &self.objects,
            suspect.into(),
            cache,
            &mut resource_cache,
            file_path,
            options,
        )?;

        Ok(outcome)
    }
}
