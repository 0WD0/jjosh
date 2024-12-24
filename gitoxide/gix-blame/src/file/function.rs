use super::{process_changes, Change, Offset, UnblamedHunk};
use crate::{BlameEntry, Error, Outcome, Statistics};
use gix_diff::blob::intern::TokenSource;
use gix_hash::ObjectId;
use gix_object::{bstr::BStr, FindExt};
use std::ops::Range;

// TODO: do not instantiate anything, get everything passed as argument.
/// Produce a list of consecutive [`BlameEntry`] instances to indicate in which commits the ranges of the file
/// at `traverse[0]:<file_path>` originated in.
///
/// ## Paramters
///
/// * `odb`
///    - Access to database objects, also for used for diffing.
///    - Should have an object cache for good diff performance.
/// * `traverse`
///    - The list of commits from the most recent to prior ones, following all parents sorted
///      by time.
///    - It's paramount that older commits are returned after newer ones.
///    - The first commit returned here is the first eligible commit to be responsible for parts of `file_path`.
/// * `file_path`
///    - A *slash-separated* worktree-relative path to the file to blame.
/// * `resource_cache`
///    - Used for diffing trees.
///
/// ## The algorithm
///
/// *For brevity, `HEAD` denotes the starting point of the blame operation. It could be any commit, or even commits that
/// represent the worktree state.
/// We begin with a single *Unblamed Hunk* and a single suspect, usually the `HEAD` commit as the commit containing the
/// *Original File*, so that it contains the entire file, with the first commit being a candidate for the entire *Original File*.
/// We traverse the commit graph starting at the first suspect, and see if there have been changes to `file_path`.
/// If so, we have found a *Blamed File* and a *Suspect* commit, and have hunks that represent these changes.
/// Now the *Unblamed Hunk* is split at the boundaries of each matching change, creating a new *Unblamed Hunk* on each side,
/// along with a [`BlameEntry`] to represent the match.
/// This is repeated until there are no non-empty *Unblamed Hunk*s left.
///
/// At a high level, what we want to do is the following:
///
/// - get the commit
/// - walk through its parents
///   - for each parent, do a diff and mark lines that don’t have a suspect yet (this is the term
///     used in `libgit2`), but that have been changed in this commit
///
/// The algorithm in `libgit2` works by going through parents and keeping a linked list of blame
/// suspects. It can be visualized as follows:
//
// <---------------------------------------->
// <---------------><----------------------->
// <---><----------><----------------------->
// <---><----------><-------><-----><------->
// <---><---><-----><-------><-----><------->
// <---><---><-----><-------><-----><-><-><->
pub fn file<E>(
    odb: impl gix_object::Find + gix_object::FindHeader,
    traverse: impl IntoIterator<Item = Result<gix_traverse::commit::Info, E>>,
    resource_cache: &mut gix_diff::blob::Platform,
    file_path: &BStr,
) -> Result<Outcome, Error>
where
    E: Into<Box<dyn std::error::Error + Send + Sync + 'static>>,
{
    let mut traverse = traverse.into_iter().peekable();
    let Some(Ok(suspect)) = traverse.peek().map(|res| res.as_ref().map(|item| item.id)) else {
        return Err(Error::EmptyTraversal);
    };
    let _span = gix_trace::coarse!("gix_blame::file()", ?file_path, ?suspect);

    let mut stats = Statistics::default();
    let (mut buf, mut buf2, mut buf3) = (Vec::new(), Vec::new(), Vec::new());
    let original_file_entry = find_path_entry_in_commit(&odb, &suspect, file_path, &mut buf, &mut buf2, &mut stats)?
        .ok_or_else(|| Error::FileMissing {
            file_path: file_path.to_owned(),
            commit_id: suspect,
        })?;
    let original_file_blob = odb.find_blob(&original_file_entry.oid, &mut buf)?.data.to_vec();
    let num_lines_in_original = {
        let mut interner = gix_diff::blob::intern::Interner::new(original_file_blob.len() / 100);
        tokens_for_diffing(&original_file_blob)
            .tokenize()
            .map(|token| interner.intern(token))
            .count()
    };

    let mut hunks_to_blame = vec![UnblamedHunk::new(
        0..num_lines_in_original as u32,
        suspect,
        Offset::Added(0),
    )];

    let mut out = Vec::new();
    let mut diff_state = gix_diff::tree::State::default();
    'outer: for item in traverse {
        let item = item.map_err(|err| Error::Traverse(err.into()))?;
        let suspect = item.id;
        stats.commits_traversed += 1;

        let mut parent_ids = item.parent_ids;
        if parent_ids.is_empty() {
            // I’m not entirely sure if this is correct yet. `suspect`, at this point, is the `id` of
            // the last `item` that was yielded by `traverse`, so it makes sense to assign the
            // remaining lines to it, even though we don’t explicitly check whether that is true
            // here. We could perhaps use `needed_to_obtain` to compare `suspect` against an empty
            // tree to validate this assumption.
            out.extend(
                hunks_to_blame
                    .iter()
                    .map(|hunk| BlameEntry::from_unblamed_hunk(hunk, suspect)),
            );

            hunks_to_blame.clear();
            break;
        }

        let Some(entry) = find_path_entry_in_commit(&odb, &suspect, file_path, &mut buf, &mut buf2, &mut stats)? else {
            continue;
        };

        if parent_ids.len() == 1 {
            let parent_id = parent_ids.pop().expect("just validated there is exactly one");
            if let Some(parent_entry) =
                find_path_entry_in_commit(&odb, &parent_id, file_path, &mut buf, &mut buf2, &mut stats)?
            {
                if entry.oid == parent_entry.oid {
                    // The blobs storing the blamed file in `entry` and `parent_entry` are identical
                    // which is why we can pass blame to the parent without further checks.
                    for unblamed_hunk in &mut hunks_to_blame {
                        unblamed_hunk.pass_blame(suspect, parent_id);
                    }
                    continue;
                }
            }

            let changes_for_file_path = tree_diff_at_file_path(
                &odb,
                file_path,
                item.id,
                parent_id,
                &mut stats,
                &mut diff_state,
                &mut buf,
                &mut buf2,
                &mut buf3,
            )?;
            let Some(modification) = changes_for_file_path else {
                // None of the changes affected the file we’re currently blaming. Pass blame to parent.
                for unblamed_hunk in &mut hunks_to_blame {
                    unblamed_hunk.pass_blame(suspect, parent_id);
                }
                continue;
            };

            match modification {
                gix_diff::tree::recorder::Change::Addition { .. } => {
                    // Every line that has not been blamed yet on a commit, is expected to have been
                    // added when the file was added to the repository.
                    out.extend(
                        hunks_to_blame
                            .iter()
                            .map(|hunk| BlameEntry::from_unblamed_hunk(hunk, suspect)),
                    );

                    hunks_to_blame.clear();
                    break;
                }
                gix_diff::tree::recorder::Change::Deletion { .. } => todo!(),
                gix_diff::tree::recorder::Change::Modification { previous_oid, oid, .. } => {
                    let changes = blob_changes(&odb, resource_cache, oid, previous_oid, file_path, &mut stats)?;
                    hunks_to_blame = process_changes(&mut out, hunks_to_blame, changes, suspect);
                    for unblamed_hunk in &mut hunks_to_blame {
                        unblamed_hunk.pass_blame(suspect, parent_id);
                    }
                }
            }
        } else {
            for parent_id in &parent_ids {
                if let Some(parent_entry) =
                    find_path_entry_in_commit(&odb, parent_id, file_path, &mut buf, &mut buf2, &mut stats)?
                {
                    if entry.oid == parent_entry.oid {
                        // The blobs storing the blamed file in `entry` and `parent_entry` are
                        // identical which is why we can pass blame to the parent without further
                        // checks.
                        for unblamed_hunk in &mut hunks_to_blame {
                            unblamed_hunk.pass_blame(suspect, *parent_id);
                        }
                        continue 'outer;
                    }
                }
            }

            for parent_id in parent_ids {
                let changes_for_file_path = tree_diff_at_file_path(
                    &odb,
                    file_path,
                    item.id,
                    parent_id,
                    &mut stats,
                    &mut diff_state,
                    &mut buf,
                    &mut buf2,
                    &mut buf3,
                )?;
                let Some(modification) = changes_for_file_path else {
                    // None of the changes affected the file we’re currently blaming. Pass blame
                    // to parent.
                    for unblamed_hunk in &mut hunks_to_blame {
                        unblamed_hunk.clone_blame(suspect, parent_id);
                    }

                    continue;
                };

                match modification {
                    gix_diff::tree::recorder::Change::Addition { .. } => {
                        // Do nothing under the assumption that this always (or almost always)
                        // implies that the file comes from a different parent, compared to which
                        // it was modified, not added.
                        //
                        // TODO: I still have to figure out whether this is correct in all cases.
                    }
                    gix_diff::tree::recorder::Change::Deletion { .. } => todo!(),
                    gix_diff::tree::recorder::Change::Modification { previous_oid, oid, .. } => {
                        let changes = blob_changes(&odb, resource_cache, oid, previous_oid, file_path, &mut stats)?;
                        hunks_to_blame = process_changes(&mut out, hunks_to_blame, changes, suspect);
                        for unblamed_hunk in &mut hunks_to_blame {
                            unblamed_hunk.pass_blame(suspect, parent_id);
                        }
                    }
                }
            }
            for unblamed_hunk in &mut hunks_to_blame {
                unblamed_hunk.remove_blame(suspect);
            }
        }
    }

    debug_assert_eq!(
        hunks_to_blame,
        vec![],
        "only if there is no portion of the file left we have completed the blame"
    );

    // I don’t know yet whether it would make sense to use a data structure instead that preserves
    // order on insertion.
    out.sort_by(|a, b| a.range_in_blamed_file.start.cmp(&b.range_in_blamed_file.start));
    Ok(Outcome {
        entries: coalesce_blame_entries(out),
        blob: original_file_blob,
        statistics: stats,
    })
}

/// This function merges adjacent blame entries. It merges entries that are adjacent both in the
/// blamed file and in the original file that introduced them. This follows `git`’s
/// behaviour. `libgit2`, as of 2024-09-19, only checks whether two entries are adjacent in the
/// blamed file which can result in different blames in certain edge cases. See [the commit][1]
/// that introduced the extra check into `git` for context. See [this commit][2] for a way to test
/// for this behaviour in `git`.
///
/// [1]: https://github.com/git/git/commit/c2ebaa27d63bfb7c50cbbdaba90aee4efdd45d0a
/// [2]: https://github.com/git/git/commit/6dbf0c7bebd1c71c44d786ebac0f2b3f226a0131
fn coalesce_blame_entries(lines_blamed: Vec<BlameEntry>) -> Vec<BlameEntry> {
    let len = lines_blamed.len();
    lines_blamed
        .into_iter()
        .fold(Vec::with_capacity(len), |mut acc, entry| {
            let previous_entry = acc.last();

            if let Some(previous_entry) = previous_entry {
                if previous_entry.commit_id == entry.commit_id
                    && previous_entry.range_in_blamed_file.end == entry.range_in_blamed_file.start
                    // As of 2024-09-19, the check below only is in `git`, but not in `libgit2`.
                    && previous_entry.range_in_original_file.end == entry.range_in_original_file.start
                {
                    let coalesced_entry = BlameEntry {
                        range_in_blamed_file: previous_entry.range_in_blamed_file.start..entry.range_in_blamed_file.end,
                        range_in_original_file: previous_entry.range_in_original_file.start
                            ..entry.range_in_original_file.end,
                        commit_id: previous_entry.commit_id,
                    };

                    acc.pop();
                    acc.push(coalesced_entry);
                } else {
                    acc.push(entry);
                }

                acc
            } else {
                acc.push(entry);

                acc
            }
        })
}

#[allow(clippy::too_many_arguments)]
fn tree_diff_at_file_path(
    odb: impl gix_object::Find + gix_object::FindHeader,
    file_path: &BStr,
    id: ObjectId,
    parent_id: ObjectId,
    stats: &mut Statistics,
    state: &mut gix_diff::tree::State,
    commit_buf: &mut Vec<u8>,
    lhs_tree_buf: &mut Vec<u8>,
    rhs_tree_buf: &mut Vec<u8>,
) -> Result<Option<gix_diff::tree::recorder::Change>, Error> {
    let parent_tree = odb.find_commit(&parent_id, commit_buf)?.tree();
    stats.commits_to_tree += 1;

    let parent_tree_iter = odb.find_tree_iter(&parent_tree, lhs_tree_buf)?;
    stats.trees_decoded += 1;

    let tree_id = odb.find_commit(&id, commit_buf)?.tree();
    stats.commits_to_tree += 1;

    let tree_iter = odb.find_tree_iter(&tree_id, rhs_tree_buf)?;
    stats.trees_decoded += 1;

    let mut recorder = gix_diff::tree::Recorder::default();
    gix_diff::tree(parent_tree_iter, tree_iter, state, &odb, &mut recorder)?;
    stats.trees_diffed += 1;

    Ok(recorder.records.into_iter().find(|change| match change {
        gix_diff::tree::recorder::Change::Modification { path, .. } => path == file_path,
        gix_diff::tree::recorder::Change::Addition { path, .. } => path == file_path,
        gix_diff::tree::recorder::Change::Deletion { path, .. } => path == file_path,
    }))
}

fn blob_changes(
    odb: impl gix_object::Find + gix_object::FindHeader,
    resource_cache: &mut gix_diff::blob::Platform,
    oid: ObjectId,
    previous_oid: ObjectId,
    file_path: &BStr,
    stats: &mut Statistics,
) -> Result<Vec<Change>, Error> {
    /// Record all [`Change`]s to learn about additions, deletions and unchanged portions of a *Blamed File*.
    struct ChangeRecorder {
        last_seen_after_end: u32,
        hunks: Vec<Change>,
        total_number_of_lines: u32,
    }

    impl ChangeRecorder {
        /// `total_number_of_lines` is used to fill in the last unchanged hunk if needed
        /// so that the entire file is represented by [`Change`].
        fn new(total_number_of_lines: u32) -> Self {
            ChangeRecorder {
                last_seen_after_end: 0,
                hunks: Vec::new(),
                total_number_of_lines,
            }
        }
    }

    impl gix_diff::blob::Sink for ChangeRecorder {
        type Out = Vec<Change>;

        fn process_change(&mut self, before: Range<u32>, after: Range<u32>) {
            // This checks for unchanged hunks.
            if after.start > self.last_seen_after_end {
                self.hunks
                    .push(Change::Unchanged(self.last_seen_after_end..after.start));
            }

            match (!before.is_empty(), !after.is_empty()) {
                (_, true) => {
                    self.hunks
                        .push(Change::Added(after.start..after.end, before.end - before.start));
                }
                (true, false) => {
                    self.hunks.push(Change::Deleted(after.start, before.end - before.start));
                }
                (false, false) => unreachable!("BUG: imara-diff provided a non-change"),
            }
            self.last_seen_after_end = after.end;
        }

        fn finish(mut self) -> Self::Out {
            if self.total_number_of_lines > self.last_seen_after_end {
                self.hunks
                    .push(Change::Unchanged(self.last_seen_after_end..self.total_number_of_lines));
            }
            self.hunks
        }
    }

    resource_cache.set_resource(
        previous_oid,
        gix_object::tree::EntryKind::Blob,
        file_path,
        gix_diff::blob::ResourceKind::OldOrSource,
        &odb,
    )?;
    resource_cache.set_resource(
        oid,
        gix_object::tree::EntryKind::Blob,
        file_path,
        gix_diff::blob::ResourceKind::NewOrDestination,
        &odb,
    )?;

    let outcome = resource_cache.prepare_diff()?;
    let input = gix_diff::blob::intern::InternedInput::new(
        tokens_for_diffing(outcome.old.data.as_slice().unwrap_or_default()),
        tokens_for_diffing(outcome.new.data.as_slice().unwrap_or_default()),
    );
    let number_of_lines_in_destination = input.after.len();
    let change_recorder = ChangeRecorder::new(number_of_lines_in_destination as u32);

    let res = gix_diff::blob::diff(gix_diff::blob::Algorithm::Histogram, &input, change_recorder);
    stats.blobs_diffed += 1;
    Ok(res)
}

fn find_path_entry_in_commit(
    odb: &impl gix_object::Find,
    commit: &gix_hash::oid,
    file_path: &BStr,
    buf: &mut Vec<u8>,
    buf2: &mut Vec<u8>,
    stats: &mut Statistics,
) -> Result<Option<gix_object::tree::Entry>, Error> {
    let commit_id = odb.find_commit(commit, buf)?.tree();
    stats.commits_to_tree += 1;
    let tree_iter = odb.find_tree_iter(&commit_id, buf)?;
    stats.trees_decoded += 1;

    let res = tree_iter.lookup_entry(
        odb,
        buf2,
        file_path.split(|b| *b == b'/').inspect(|_| stats.trees_decoded += 1),
    )?;
    stats.trees_decoded -= 1;
    Ok(res)
}

/// Return an iterator over tokens for use in diffing. These usually lines, but iit's important to unify them
/// so the later access shows the right thing.
pub(crate) fn tokens_for_diffing(data: &[u8]) -> impl TokenSource<Token = &[u8]> {
    gix_diff::blob::sources::byte_lines_with_terminator(data)
}
