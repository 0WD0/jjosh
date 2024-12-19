//! A crate to implement an algorithm to annotate lines in tracked files with the commits that changed them.
//!
//! ### Terminology
//!
//! * **Original File**
//!    - The file as it exists in `HEAD`.
//!    - the initial state with all lines that we need to associate with a *Blamed File*.
//! * **Blamed File**
//!    - A file at a version (i.e. commit) that introduces hunks into the final 'image'.
//! * **Suspects**
//!    - The versions of the files that can contain hunks that we could use in the final 'image'
//!    - multiple at the same time as the commit-graph may split up.
//!    - turns into *Blamed File* once we have found an association into the *Original File*.
//!    - every [`UnblamedHunk`] can have multiple suspects of which we find the best match.
#![deny(rust_2018_idioms, missing_docs)]
#![forbid(unsafe_code)]

use std::{
    collections::BTreeMap,
    ops::{AddAssign, Range, SubAssign},
    path::PathBuf,
};

use gix_hash::ObjectId;
use gix_object::bstr::BStr;
use gix_object::FindExt;

/// Describes the offset of a particular hunk relative to the *Original File*.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Offset {
    /// The amount of lines to add.
    Added(u32),
    /// The amount of lines to remove.
    Deleted(u32),
}

impl Offset {
    /// Shift the given `range` according to our offset.
    pub fn shifted_range(&self, range: &Range<u32>) -> Range<u32> {
        match self {
            Offset::Added(added) => {
                debug_assert!(range.start >= *added, "{self:?} {range:?}");
                Range {
                    start: range.start - added,
                    end: range.end - added,
                }
            }
            Offset::Deleted(deleted) => Range {
                start: range.start + deleted,
                end: range.end + deleted,
            },
        }
    }
}

impl AddAssign<u32> for Offset {
    fn add_assign(&mut self, rhs: u32) {
        match self {
            Self::Added(added) => *self = Self::Added(*added + rhs),
            Self::Deleted(deleted) => {
                if rhs > *deleted {
                    *self = Self::Added(rhs - *deleted);
                } else {
                    *self = Self::Deleted(*deleted - rhs);
                }
            }
        }
    }
}

impl SubAssign<u32> for Offset {
    fn sub_assign(&mut self, rhs: u32) {
        match self {
            Self::Added(added) => {
                if rhs > *added {
                    *self = Self::Deleted(rhs - *added);
                } else {
                    *self = Self::Added(*added - rhs);
                }
            }
            Self::Deleted(deleted) => *self = Self::Deleted(*deleted + rhs),
        }
    }
}

/// A mapping of a section of the *Original File* to the section in a *Blamed File* that introduced it.
///
/// Both ranges are of the same size, but may use different [starting points](Range::start). Naturally,
/// they have the same content, which is the reason they are in what is returned by [`file()`].
// TODO: see if this can be encoded as `start_in_original_file` and `start_in_blamed_file` and a single `len`.
#[derive(Debug, PartialEq)]
pub struct BlameEntry {
    /// The section of tokens in the tokenized version of the *Blamed File* (typically lines).
    pub range_in_blamed_file: Range<u32>,
    /// The section of tokens in the tokenized version of the *Original File* (typically lines).
    pub range_in_original_file: Range<u32>,
    /// The commit that introduced the section into the *Blamed File*.
    pub commit_id: ObjectId,
}

impl BlameEntry {
    /// Create a new instance.
    pub fn new(range_in_blamed_file: Range<u32>, range_in_original_file: Range<u32>, commit_id: ObjectId) -> Self {
        debug_assert!(
            range_in_blamed_file.end > range_in_blamed_file.start,
            "{range_in_blamed_file:?}"
        );
        debug_assert!(
            range_in_original_file.end > range_in_original_file.start,
            "{range_in_original_file:?}"
        );
        debug_assert_eq!(range_in_original_file.len(), range_in_blamed_file.len());

        Self {
            range_in_blamed_file: range_in_blamed_file.clone(),
            range_in_original_file: range_in_original_file.clone(),
            commit_id,
        }
    }

    /// Create a new instance by creating `range_in_blamed_file` after applying `offset` to `range_in_original_file`.
    fn with_offset(range_in_original_file: Range<u32>, commit_id: ObjectId, offset: Offset) -> Self {
        debug_assert!(
            range_in_original_file.end > range_in_original_file.start,
            "{range_in_original_file:?}"
        );

        match offset {
            Offset::Added(added) => Self {
                range_in_blamed_file: (range_in_original_file.start + added)..(range_in_original_file.end + added),
                range_in_original_file,
                commit_id,
            },
            Offset::Deleted(deleted) => {
                debug_assert!(
                    range_in_original_file.start >= deleted,
                    "{range_in_original_file:?} {offset:?}"
                );

                Self {
                    range_in_blamed_file: (range_in_original_file.start - deleted)
                        ..(range_in_original_file.end - deleted),
                    range_in_original_file,
                    commit_id,
                }
            }
        }
    }

    /// Create an offset from a portion of the *Original File*.
    fn from_unblamed_hunk(unblamed_hunk: &UnblamedHunk, commit_id: ObjectId) -> Self {
        let range_in_original_file = unblamed_hunk.suspects.get(&commit_id).unwrap();

        Self {
            range_in_blamed_file: unblamed_hunk.range_in_blamed_file.clone(),
            range_in_original_file: range_in_original_file.clone(),
            commit_id,
        }
    }
}

trait LineRange {
    fn shift_by(&self, offset: Offset) -> Self;
}

impl LineRange for Range<u32> {
    fn shift_by(&self, offset: Offset) -> Self {
        offset.shifted_range(self)
    }
}

/// TODO: docs - what is it?
// TODO: is `Clone` really needed.
#[derive(Clone, Debug, PartialEq)]
pub struct UnblamedHunk {
    /// TODO: figure out how this works.
    pub range_in_blamed_file: Range<u32>,
    /// Maps a commit to the range in the *Original File* that `range_in_blamed_file` refers to.
    pub suspects: BTreeMap<ObjectId, Range<u32>>,
}

#[derive(Debug)]
enum Either<T, U> {
    Left(T),
    Right(U),
}

impl UnblamedHunk {
    fn new(range_in_blamed_file: Range<u32>, suspect: ObjectId, offset: Offset) -> Self {
        assert!(
            range_in_blamed_file.end > range_in_blamed_file.start,
            "{range_in_blamed_file:?}"
        );

        let range_in_destination = range_in_blamed_file.shift_by(offset);

        Self {
            range_in_blamed_file,
            suspects: [(suspect, range_in_destination)].into(),
        }
    }

    fn shift_by(mut self, suspect: ObjectId, offset: Offset) -> Self {
        self.suspects.entry(suspect).and_modify(|e| *e = e.shift_by(offset));

        self
    }

    fn split_at(self, suspect: ObjectId, line_number_in_destination: u32) -> Either<Self, (Self, Self)> {
        match self.suspects.get(&suspect) {
            None => Either::Left(self),
            Some(range_in_suspect) => {
                if line_number_in_destination > range_in_suspect.start
                    && line_number_in_destination < range_in_suspect.end
                {
                    let split_at_from_start = line_number_in_destination - range_in_suspect.start;

                    if split_at_from_start > 0 {
                        let new_suspects_before = self
                            .suspects
                            .iter()
                            .map(|(suspect, range)| (*suspect, range.start..(range.start + split_at_from_start)))
                            .collect();

                        let new_suspects_after = self
                            .suspects
                            .iter()
                            .map(|(suspect, range)| (*suspect, (range.start + split_at_from_start)..range.end))
                            .collect();

                        let new_hunk_before = Self {
                            range_in_blamed_file: self.range_in_blamed_file.start
                                ..(self.range_in_blamed_file.start + split_at_from_start),
                            suspects: new_suspects_before,
                        };
                        let new_hunk_after = Self {
                            range_in_blamed_file: (self.range_in_blamed_file.start + split_at_from_start)
                                ..(self.range_in_blamed_file.end),
                            suspects: new_suspects_after,
                        };

                        Either::Right((new_hunk_before, new_hunk_after))
                    } else {
                        Either::Left(self)
                    }
                } else {
                    Either::Left(self)
                }
            }
        }
    }

    fn offset_for(&self, suspect: ObjectId) -> Offset {
        let range_in_suspect = self.suspects.get(&suspect).expect("TODO");

        if self.range_in_blamed_file.start > range_in_suspect.start {
            Offset::Added(self.range_in_blamed_file.start - range_in_suspect.start)
        } else {
            Offset::Deleted(range_in_suspect.start - self.range_in_blamed_file.start)
        }
    }

    /// Transfer all ranges from the commit at `from` to the commit at `to`.
    fn pass_blame(&mut self, from: ObjectId, to: ObjectId) {
        if let Some(range_in_suspect) = self.suspects.remove(&from) {
            self.suspects.insert(to, range_in_suspect);
        }
    }

    fn clone_blame(&mut self, from: ObjectId, to: ObjectId) {
        if let Some(range_in_suspect) = self.suspects.get(&from) {
            self.suspects.insert(to, range_in_suspect.clone());
        }
    }

    fn remove_blame(&mut self, suspect: ObjectId) {
        // TODO: figure out why it can try to remove suspects that don't exist.
        self.suspects.remove(&suspect);
    }
}

/// A single change between two blobs, or an unchanged region.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    /// A range of tokens that wasn't changed.
    Unchanged(Range<u32>),
    /// `(added_line_range, num_deleted_in_before)`
    Added(Range<u32>, u32),
    /// `(line_to_start_deletion_at, num_deleted_in_before)`
    Deleted(u32, u32),
}

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

/// Compare a section from the *Original File* (`hunk`) with a change from a diff and see if there
/// is an intersection with `change`. Based on that intersection, we may generate a [`BlameEntry`] for `out`
/// and/or split the `hunk` into multiple.
///
/// This is the core of the blame implementation as it matches regions in *Blamed Files* to the *Original File*.
pub fn process_change(
    out: &mut Vec<BlameEntry>,
    new_hunks_to_blame: &mut Vec<UnblamedHunk>,
    offset_in_destination: &mut Offset,
    suspect: ObjectId,
    hunk: Option<UnblamedHunk>,
    change: Option<Change>,
) -> (Option<UnblamedHunk>, Option<Change>) {
    match (hunk, change) {
        (Some(hunk), Some(Change::Unchanged(unchanged))) => {
            let Some(range_in_suspect) = hunk.suspects.get(&suspect) else {
                new_hunks_to_blame.push(hunk);
                return (None, Some(Change::Unchanged(unchanged)));
            };

            match (
                // Since `unchanged` is a range that is not inclusive at the end,
                // `unchanged.end` is not part of `unchanged`. The first line that is
                // `unchanged.end - 1`.
                range_in_suspect.contains(&unchanged.start),
                (unchanged.end - 1) >= range_in_suspect.start && unchanged.end <= range_in_suspect.end,
            ) {
                (_, true) => {
                    //     <------>  (hunk)
                    // <------->     (unchanged)
                    //
                    // <---------->  (hunk)
                    //     <--->     (unchanged)

                    (Some(hunk), None)
                }
                (true, false) => {
                    // <-------->     (hunk)
                    //     <------->  (unchanged)

                    new_hunks_to_blame.push(hunk.shift_by(suspect, *offset_in_destination));

                    (None, Some(Change::Unchanged(unchanged)))
                }
                (false, false) => {
                    // Any of the following cases are handled by this branch:
                    //    <--->      (hunk)
                    // <---------->  (unchanged)
                    //
                    //       <---->  (hunk)
                    // <-->          (unchanged)
                    //
                    // <-->          (hunk)
                    //       <---->  (unchanged)

                    if unchanged.end <= range_in_suspect.start {
                        //       <---->  (hunk)
                        // <-->          (unchanged)

                        (Some(hunk.clone()), None)
                    } else {
                        // <-->          (hunk)
                        //       <---->  (unchanged)
                        //
                        //    <--->      (hunk)
                        // <---------->  (unchanged)

                        new_hunks_to_blame.push(hunk.shift_by(suspect, *offset_in_destination));

                        (None, Some(Change::Unchanged(unchanged.clone())))
                    }
                }
            }
        }
        (Some(hunk), Some(Change::Added(added, number_of_lines_deleted))) => {
            let Some(range_in_suspect) = hunk.suspects.get(&suspect) else {
                new_hunks_to_blame.push(hunk);

                return (None, Some(Change::Added(added, number_of_lines_deleted)));
            };

            let range_in_suspect = range_in_suspect.clone();

            match (
                range_in_suspect.contains(&added.start),
                // Since `added` is a range that is not inclusive at the end, `added.end` is
                // not part of `added`. The first line that is `added.end - 1`.
                (added.end - 1) >= range_in_suspect.start && added.end <= range_in_suspect.end,
            ) {
                (true, true) => {
                    // <---------->  (hunk)
                    //     <--->     (added)
                    //     <--->     (blamed)
                    // <-->     <->  (new hunk)

                    let new_hunk = match hunk.split_at(suspect, added.start) {
                        Either::Left(hunk) => hunk,
                        Either::Right((before, after)) => {
                            new_hunks_to_blame.push(before.shift_by(suspect, *offset_in_destination));

                            after
                        }
                    };

                    *offset_in_destination += added.end - added.start;
                    *offset_in_destination -= number_of_lines_deleted;

                    out.push(BlameEntry::with_offset(
                        added.clone(),
                        suspect,
                        new_hunk.offset_for(suspect),
                    ));

                    match new_hunk.split_at(suspect, added.end) {
                        Either::Left(_) => (None, None),
                        Either::Right((_, after)) => (Some(after), None),
                    }
                }
                (true, false) => {
                    // <-------->     (hunk)
                    //     <------->  (added)
                    //     <---->     (blamed)
                    // <-->           (new hunk)

                    let new_hunk = match hunk.split_at(suspect, added.start) {
                        Either::Left(hunk) => hunk,
                        Either::Right((before, after)) => {
                            new_hunks_to_blame.push(before.shift_by(suspect, *offset_in_destination));

                            after
                        }
                    };

                    out.push(BlameEntry::with_offset(
                        added.start..range_in_suspect.end,
                        suspect,
                        new_hunk.offset_for(suspect),
                    ));

                    if added.end > range_in_suspect.end {
                        (None, Some(Change::Added(added, number_of_lines_deleted)))
                    } else {
                        todo!();
                    }
                }
                (false, true) => {
                    //    <------->  (hunk)
                    // <------>      (added)
                    //    <--->      (blamed)
                    //         <-->  (new hunk)

                    out.push(BlameEntry::with_offset(
                        range_in_suspect.start..added.end,
                        suspect,
                        hunk.offset_for(suspect),
                    ));

                    *offset_in_destination += added.end - added.start;
                    *offset_in_destination -= number_of_lines_deleted;

                    match hunk.split_at(suspect, added.end) {
                        Either::Left(_) => (None, None),
                        Either::Right((_, after)) => (Some(after), None),
                    }
                }
                (false, false) => {
                    // Any of the following cases are handled by this branch:
                    //    <--->      (hunk)
                    // <---------->  (added)
                    //
                    //       <---->  (hunk)
                    // <-->          (added)
                    //
                    // <-->          (hunk)
                    //       <---->  (added)

                    if added.end <= range_in_suspect.start {
                        //       <---->  (hunk)
                        // <-->          (added)

                        *offset_in_destination += added.end - added.start;
                        *offset_in_destination -= number_of_lines_deleted;

                        (Some(hunk.clone()), None)
                    } else if range_in_suspect.end <= added.start {
                        // <-->          (hunk)
                        //       <---->  (added)

                        new_hunks_to_blame.push(hunk.shift_by(suspect, *offset_in_destination));

                        (None, Some(Change::Added(added.clone(), number_of_lines_deleted)))
                    } else {
                        //    <--->      (hunk)
                        // <---------->  (added)
                        //    <--->      (blamed)

                        out.push(BlameEntry::with_offset(
                            range_in_suspect.clone(),
                            suspect,
                            hunk.offset_for(suspect),
                        ));

                        (None, Some(Change::Added(added.clone(), number_of_lines_deleted)))
                    }
                }
            }
        }
        (Some(hunk), Some(Change::Deleted(line_number_in_destination, number_of_lines_deleted))) => {
            let range_in_suspect = hunk.suspects.get(&suspect).expect("TODO");

            if line_number_in_destination < range_in_suspect.start {
                //     <--->  (hunk)
                //  |         (line_number_in_destination)

                *offset_in_destination -= number_of_lines_deleted;

                (Some(hunk), None)
            } else if line_number_in_destination < range_in_suspect.end {
                //  <----->  (hunk)
                //     |     (line_number_in_destination)

                let new_hunk = match hunk.split_at(suspect, line_number_in_destination) {
                    Either::Left(hunk) => hunk,
                    Either::Right((before, after)) => {
                        new_hunks_to_blame.push(before.shift_by(suspect, *offset_in_destination));

                        after
                    }
                };

                *offset_in_destination -= number_of_lines_deleted;

                (Some(new_hunk), None)
            } else {
                //  <--->     (hunk)
                //         |  (line_number_in_destination)

                new_hunks_to_blame.push(hunk.shift_by(suspect, *offset_in_destination));

                (
                    None,
                    Some(Change::Deleted(line_number_in_destination, number_of_lines_deleted)),
                )
            }
        }
        (Some(hunk), None) => {
            new_hunks_to_blame.push(hunk.shift_by(suspect, *offset_in_destination));

            (None, None)
        }
        (None, Some(Change::Unchanged(_))) => (None, None),
        (None, Some(Change::Added(added, number_of_lines_deleted))) => {
            *offset_in_destination += added.end - added.start;
            *offset_in_destination -= number_of_lines_deleted;

            (None, None)
        }
        (None, Some(Change::Deleted(_, number_of_lines_deleted))) => {
            *offset_in_destination -= number_of_lines_deleted;

            (None, None)
        }
        (None, None) => (None, None),
    }
}

/// Consume `hunks_to_blame` and `changes` to pair up matches ranges (also overlapping) with each other.
/// Once a match is found, it's pushed onto `out`.
pub fn process_changes(
    out: &mut Vec<BlameEntry>,
    hunks_to_blame: Vec<UnblamedHunk>,
    changes: Vec<Change>,
    suspect: ObjectId,
) -> Vec<UnblamedHunk> {
    let mut hunks_iter = hunks_to_blame.into_iter();
    let mut changes_iter = changes.into_iter();

    let mut hunk = hunks_iter.next();
    let mut change = changes_iter.next();

    let mut new_hunks_to_blame = Vec::new();
    let mut offset_in_destination = Offset::Added(0);

    loop {
        (hunk, change) = process_change(
            out,
            &mut new_hunks_to_blame,
            &mut offset_in_destination,
            suspect,
            hunk,
            change,
        );

        hunk = hunk.or_else(|| hunks_iter.next());
        change = change.or_else(|| changes_iter.next());

        if hunk.is_none() && change.is_none() {
            break;
        }
    }
    new_hunks_to_blame
}

fn tree_diff_at_file_path(
    odb: impl gix_object::Find + gix_object::FindHeader,
    file_path: &BStr,
    id: ObjectId,
    parent_id: ObjectId,
) -> Option<gix_diff::tree::recorder::Change> {
    let mut buffer = Vec::new();

    let parent = odb.find_commit(&parent_id, &mut buffer).unwrap();

    let mut buffer = Vec::new();
    let parent_tree_iter = odb
        .find(&parent.tree(), &mut buffer)
        .unwrap()
        .try_into_tree_iter()
        .unwrap();

    let mut buffer = Vec::new();
    let commit = odb.find_commit(&id, &mut buffer).unwrap();

    let mut buffer = Vec::new();
    let tree_iter = odb
        .find(&commit.tree(), &mut buffer)
        .unwrap()
        .try_into_tree_iter()
        .unwrap();

    let mut recorder = gix_diff::tree::Recorder::default();
    gix_diff::tree(
        parent_tree_iter,
        tree_iter,
        gix_diff::tree::State::default(),
        &odb,
        &mut recorder,
    )
    .unwrap();

    recorder.records.into_iter().find(|change| match change {
        gix_diff::tree::recorder::Change::Modification { path, .. } => path == file_path,
        gix_diff::tree::recorder::Change::Addition { path, .. } => path == file_path,
        gix_diff::tree::recorder::Change::Deletion { path, .. } => path == file_path,
    })
}

fn blob_changes(
    odb: impl gix_object::Find + gix_object::FindHeader,
    resource_cache: &mut gix_diff::blob::Platform,
    oid: ObjectId,
    previous_oid: ObjectId,
    file_path: &BStr,
) -> Vec<Change> {
    resource_cache
        .set_resource(
            previous_oid,
            gix_object::tree::EntryKind::Blob,
            file_path,
            gix_diff::blob::ResourceKind::OldOrSource,
            &odb,
        )
        .unwrap();
    resource_cache
        .set_resource(
            oid,
            gix_object::tree::EntryKind::Blob,
            file_path,
            gix_diff::blob::ResourceKind::NewOrDestination,
            &odb,
        )
        .unwrap();

    let outcome = resource_cache.prepare_diff().unwrap();
    let input = outcome.interned_input();
    let number_of_lines_in_destination = input.after.len();
    let change_recorder = ChangeRecorder::new(number_of_lines_in_destination.try_into().unwrap());

    gix_diff::blob::diff(gix_diff::blob::Algorithm::Histogram, &input, change_recorder)
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
/// We begin with a single [`UnblamedHunk`] and a single suspect, usually `HEAD` as the commit containing the *Original File*.
/// We traverse the commit graph starting at `HEAD`, and see if there have been changes to `file_path`. If so, we have found
/// a *Blamed File* and a *Suspect* commit, and have hunks that represent these changes. Now the [`UnblamedHunk`]s is split at
/// the boundaries of each matching hunk, creating a new [`UnblamedHunk`] on each side, along with a [`BlameEntry`] to represent
/// the match.
/// This is repeated until there are no non-empty [`UnblamedHunk`]s left.
///
/// At a high level, what we want to do is the following:
///
/// - get the commit that belongs to a commit id
/// - walk through parents
///   - for each parent, do a diff and mark lines that don’t have a suspect (this is the term
///     used in `libgit2`) yet, but that have been changed in this commit
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
    // TODO: remove
    worktree_root: PathBuf,
    file_path: &BStr,
) -> Result<Vec<BlameEntry>, E> {
    // TODO: `worktree_root` should be removed - read everything from Commit.
    //       Worktree changes should be placed into a temporary commit.
    // TODO: remove this and deduplicate the respective code.
    use gix_object::bstr::ByteSlice;
    let absolute_path = worktree_root.join(gix_path::from_bstr(file_path));

    // TODO  use `imara-diff` to tokenize this just like it will be tokenized when diffing.
    let number_of_lines = std::fs::read_to_string(absolute_path).unwrap().lines().count();

    let mut traverse = traverse.into_iter().peekable();
    let Some(Ok(suspect)) = traverse.peek().map(|res| res.as_ref().map(|item| item.id)) else {
        todo!("return actual error");
    };

    let mut hunks_to_blame = vec![UnblamedHunk::new(
        0..number_of_lines.try_into().unwrap(),
        suspect,
        Offset::Added(0),
    )];

    let mut out = Vec::new();
    'outer: for item in traverse {
        let item = item?;
        let suspect = item.id;

        let parent_ids = item.parent_ids;
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

        let mut buffer = Vec::new();
        let commit_id = odb.find_commit(&suspect, &mut buffer).unwrap().tree();
        let tree_iter = odb.find_tree_iter(&commit_id, &mut buffer).unwrap();

        let mut entry_buffer = Vec::new();
        let Some(entry) = tree_iter
            .lookup_entry_by_path(&odb, &mut entry_buffer, file_path.to_str().unwrap())
            .unwrap()
        else {
            continue;
        };

        if parent_ids.len() == 1 {
            let parent_id: ObjectId = *parent_ids.last().unwrap();

            let mut buffer = Vec::new();
            let parent_commit_id = odb.find_commit(&parent_id, &mut buffer).unwrap().tree();
            let parent_tree_iter = odb.find_tree_iter(&parent_commit_id, &mut buffer).unwrap();

            let mut entry_buffer = Vec::new();
            if let Some(parent_entry) = parent_tree_iter
                .lookup_entry_by_path(&odb, &mut entry_buffer, file_path.to_str().unwrap())
                .unwrap()
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

            let Some(modification) = tree_diff_at_file_path(&odb, file_path, item.id, parent_id) else {
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
                    let changes = blob_changes(&odb, resource_cache, oid, previous_oid, file_path);

                    hunks_to_blame = process_changes(&mut out, hunks_to_blame, changes, suspect);
                    for unblamed_hunk in &mut hunks_to_blame {
                        unblamed_hunk.pass_blame(suspect, parent_id);
                    }
                }
            }
        } else {
            let mut buffer = Vec::new();
            let commit_id = odb.find_commit(&suspect, &mut buffer).unwrap().tree();
            let tree_iter = odb.find_tree_iter(&commit_id, &mut buffer).unwrap();

            let mut entry_buffer = Vec::new();
            let entry = tree_iter
                .lookup_entry_by_path(&odb, &mut entry_buffer, file_path.to_str().unwrap())
                .unwrap()
                .unwrap();

            for parent_id in &parent_ids {
                let mut buffer = Vec::new();
                let parent_commit_id = odb.find_commit(parent_id, &mut buffer).unwrap().tree();
                let parent_tree_iter = odb.find_tree_iter(&parent_commit_id, &mut buffer).unwrap();

                let mut entry_buffer = Vec::new();
                if let Some(parent_entry) = parent_tree_iter
                    .lookup_entry_by_path(&odb, &mut entry_buffer, file_path.to_str().unwrap())
                    .unwrap()
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
                let changes_for_file_path = tree_diff_at_file_path(&odb, file_path, item.id, parent_id);
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
                        let changes = blob_changes(&odb, resource_cache, oid, previous_oid, file_path);

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
    Ok(coalesce_blame_entries(out))
}
