//! A module with low-level types and functions.
use std::ops::Range;

use gix_hash::ObjectId;

use crate::types::{BlameEntry, Either, LineRange};
use crate::types::{Change, Offset, UnblamedHunk};

pub(super) mod function;

/// Compare a section from the *Original File* (`hunk`) with a change from a diff and see if there
/// is an intersection with `change`. Based on that intersection, we may generate a [`BlameEntry`] for `out`
/// and/or split the `hunk` into multiple.
///
/// This is the core of the blame implementation as it matches regions in *Blamed Files* to the *Original File*.
fn process_change(
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
            let range_in_suspect = hunk
                .suspects
                .get(&suspect)
                .expect("Internal and we know suspect is present");

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
fn process_changes(
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
        let range_in_suspect = self
            .suspects
            .get(&suspect)
            .expect("Internal and we know suspect is present");

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

impl BlameEntry {
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
        let range_in_original_file = unblamed_hunk
            .suspects
            .get(&commit_id)
            .expect("Private and only called when we now `commit_id` is in the suspect list");

        Self {
            range_in_blamed_file: unblamed_hunk.range_in_blamed_file.clone(),
            range_in_original_file: range_in_original_file.clone(),
            commit_id,
        }
    }
}

#[cfg(test)]
mod tests;
