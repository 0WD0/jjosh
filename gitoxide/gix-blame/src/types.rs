use std::{
    collections::BTreeMap,
    ops::{AddAssign, Range, SubAssign},
};

use crate::file::function::tokens_for_diffing;
use gix_hash::ObjectId;
use gix_object::bstr::BString;

/// The outcome of [`file()`](crate::file()).
#[derive(Debug, Clone)]
pub struct Outcome {
    /// One entry in sequential order, to associate a hunk in the original file with the commit (and its lines)
    /// that introduced it.
    pub entries: Vec<BlameEntry>,
    /// A buffer with the file content of the *Original File*, ready for tokenization.
    pub blob: Vec<u8>,
    /// Additional information about the amount of work performed to produce the blame.
    pub statistics: Statistics,
}

/// Additional information about the performed operations.
#[derive(Debug, Default, Copy, Clone)]
pub struct Statistics {
    /// The amount of commits it traversed until the blame was complete.
    pub commits_traversed: usize,
    /// The amount of commits whose trees were extracted.
    pub commits_to_tree: usize,
    /// The amount of trees that were decoded to find the entry of the file to blame.
    pub trees_decoded: usize,
    /// The amount of fully-fledged tree-diffs to see if the filepath was added, deleted or modified.
    pub trees_diffed: usize,
    /// The amount of blobs there were compared to each other to learn what changed between commits.
    /// Note that in order to diff a blob, one needs to load both versions from the database.
    pub blobs_diffed: usize,
}

impl Outcome {
    /// Return an iterator over each entry in [`Self::entries`], along with its lines, line by line.
    ///
    /// Note that [`Self::blob`] must be tokenized in exactly the same way as the tokenizer that was used
    /// to perform the diffs, which is what this method assures.
    pub fn entries_with_lines(&self) -> impl Iterator<Item = (BlameEntry, Vec<BString>)> + '_ {
        use gix_diff::blob::intern::TokenSource;
        let mut interner = gix_diff::blob::intern::Interner::new(self.blob.len() / 100);
        let lines_as_tokens: Vec<_> = tokens_for_diffing(&self.blob)
            .tokenize()
            .map(|token| interner.intern(token))
            .collect();
        self.entries.iter().map(move |e| {
            let Range { start, end } = e.range_in_blamed_file.clone();
            (
                e.clone(),
                lines_as_tokens[start as usize..end as usize]
                    .iter()
                    .map(|token| BString::new(interner[*token].into()))
                    .collect(),
            )
        })
    }
}

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
/// they have the same content, which is the reason they are in what is returned by [`file()`](crate::file()).
// TODO: see if this can be encoded as `start_in_original_file` and `start_in_blamed_file` and a single `len`.
#[derive(Clone, Debug, PartialEq)]
pub struct BlameEntry {
    /// The section of tokens in the tokenized version of the *Blamed File* (typically lines).
    pub range_in_blamed_file: Range<u32>,
    /// The section of tokens in the tokenized version of the *Original File* (typically lines).
    // TODO: figure out why this is basically inverted. Probably that's just it - would make sense with `UnblamedHunk` then.
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
}

pub(crate) trait LineRange {
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
pub(crate) enum Either<T, U> {
    Left(T),
    Right(U),
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
