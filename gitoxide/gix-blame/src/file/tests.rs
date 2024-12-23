use crate::file::{Offset, UnblamedHunk};
use gix_hash::ObjectId;
use std::ops::Range;

fn new_unblamed_hunk(range_in_blamed_file: Range<u32>, suspect: ObjectId, offset: Offset) -> UnblamedHunk {
    assert!(
        range_in_blamed_file.end > range_in_blamed_file.start,
        "{range_in_blamed_file:?}"
    );

    let range_in_destination = offset.shifted_range(&range_in_blamed_file);
    UnblamedHunk {
        range_in_blamed_file,
        suspects: [(suspect, range_in_destination)].into(),
    }
}

mod process_change {
    use super::*;
    use crate::file::{process_change, Change, Offset, UnblamedHunk};
    use crate::BlameEntry;
    use gix_hash::ObjectId;

    #[test]
    fn nothing() {
        let mut lines_blamed = Vec::new();
        let mut new_hunks_to_blame = Vec::new();
        let mut offset_in_destination: Offset = Offset::Added(0);
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);

        let (hunk, change) = process_change(
            &mut lines_blamed,
            &mut new_hunks_to_blame,
            &mut offset_in_destination,
            suspect,
            None,
            None,
        );

        assert_eq!(hunk, None);
        assert_eq!(change, None);
        assert_eq!(offset_in_destination, Offset::Added(0));
    }

    #[test]
    fn added_hunk() {
        let mut lines_blamed = Vec::new();
        let mut new_hunks_to_blame = Vec::new();
        let mut offset_in_destination: Offset = Offset::Added(0);
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);

        let (hunk, change) = process_change(
            &mut lines_blamed,
            &mut new_hunks_to_blame,
            &mut offset_in_destination,
            suspect,
            Some(new_unblamed_hunk(0..5, suspect, Offset::Added(0))),
            Some(Change::Added(0..3, 0)),
        );

        assert_eq!(
            hunk,
            Some(UnblamedHunk {
                range_in_blamed_file: 3..5,
                suspects: [(suspect, 3..5)].into()
            })
        );
        assert_eq!(change, None);
        assert_eq!(
            lines_blamed,
            [BlameEntry {
                range_in_blamed_file: 0..3,
                range_in_original_file: 0..3,
                commit_id: suspect
            }]
        );
        assert_eq!(new_hunks_to_blame, []);
        assert_eq!(offset_in_destination, Offset::Added(3));
    }

    #[test]
    fn added_hunk_2() {
        let mut lines_blamed = Vec::new();
        let mut new_hunks_to_blame = Vec::new();
        let mut offset_in_destination: Offset = Offset::Added(0);
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);

        let (hunk, change) = process_change(
            &mut lines_blamed,
            &mut new_hunks_to_blame,
            &mut offset_in_destination,
            suspect,
            Some(new_unblamed_hunk(0..5, suspect, Offset::Added(0))),
            Some(Change::Added(2..3, 0)),
        );

        assert_eq!(
            hunk,
            Some(UnblamedHunk {
                range_in_blamed_file: 3..5,
                suspects: [(suspect, 3..5)].into()
            })
        );
        assert_eq!(change, None);
        assert_eq!(
            lines_blamed,
            [BlameEntry {
                range_in_blamed_file: 2..3,
                range_in_original_file: 2..3,
                commit_id: suspect
            }]
        );
        assert_eq!(
            new_hunks_to_blame,
            [UnblamedHunk {
                range_in_blamed_file: 0..2,
                suspects: [(suspect, 0..2)].into()
            }]
        );
        assert_eq!(offset_in_destination, Offset::Added(1));
    }

    #[test]
    fn added_hunk_3() {
        let mut lines_blamed = Vec::new();
        let mut new_hunks_to_blame = Vec::new();
        let mut offset_in_destination: Offset = Offset::Added(5);
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);

        let (hunk, change) = process_change(
            &mut lines_blamed,
            &mut new_hunks_to_blame,
            &mut offset_in_destination,
            suspect,
            Some(new_unblamed_hunk(10..15, suspect, Offset::Added(0))),
            Some(Change::Added(12..13, 0)),
        );

        assert_eq!(
            hunk,
            Some(UnblamedHunk {
                range_in_blamed_file: 13..15,
                suspects: [(suspect, 13..15)].into()
            })
        );
        assert_eq!(change, None);
        assert_eq!(
            lines_blamed,
            [BlameEntry {
                range_in_blamed_file: 12..13,
                range_in_original_file: 12..13,
                commit_id: suspect
            }]
        );
        assert_eq!(
            new_hunks_to_blame,
            [UnblamedHunk {
                range_in_blamed_file: 10..12,
                suspects: [(suspect, 5..7)].into()
            }]
        );
        assert_eq!(offset_in_destination, Offset::Added(6));
    }

    #[test]
    fn added_hunk_4() {
        let mut lines_blamed = Vec::new();
        let mut new_hunks_to_blame = Vec::new();
        let mut offset_in_destination: Offset = Offset::Added(0);
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);

        let (hunk, change) = process_change(
            &mut lines_blamed,
            &mut new_hunks_to_blame,
            &mut offset_in_destination,
            suspect,
            // range_in_destination: 7..12
            Some(new_unblamed_hunk(12..17, suspect, Offset::Added(5))),
            Some(Change::Added(9..10, 0)),
        );

        assert_eq!(
            hunk,
            Some(UnblamedHunk {
                range_in_blamed_file: 15..17,
                suspects: [(suspect, 10..12)].into()
            })
        );
        assert_eq!(change, None);
        assert_eq!(
            lines_blamed,
            [BlameEntry {
                range_in_blamed_file: 14..15,
                range_in_original_file: 9..10,
                commit_id: suspect
            }]
        );
        assert_eq!(
            new_hunks_to_blame,
            [UnblamedHunk {
                range_in_blamed_file: 12..14,
                suspects: [(suspect, 7..9)].into()
            }]
        );
        assert_eq!(offset_in_destination, Offset::Added(1));
    }

    #[test]
    fn added_hunk_5() {
        let mut lines_blamed = Vec::new();
        let mut new_hunks_to_blame = Vec::new();
        let mut offset_in_destination: Offset = Offset::Added(0);
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);

        let (hunk, change) = process_change(
            &mut lines_blamed,
            &mut new_hunks_to_blame,
            &mut offset_in_destination,
            suspect,
            Some(new_unblamed_hunk(0..5, suspect, Offset::Added(0))),
            Some(Change::Added(0..3, 1)),
        );

        assert_eq!(
            hunk,
            Some(UnblamedHunk {
                range_in_blamed_file: 3..5,
                suspects: [(suspect, 3..5)].into()
            })
        );
        assert_eq!(change, None);
        assert_eq!(
            lines_blamed,
            [BlameEntry {
                range_in_blamed_file: 0..3,
                range_in_original_file: 0..3,
                commit_id: suspect
            }]
        );
        assert_eq!(new_hunks_to_blame, []);
        assert_eq!(offset_in_destination, Offset::Added(2));
    }

    #[test]
    fn added_hunk_6() {
        let mut lines_blamed = Vec::new();
        let mut new_hunks_to_blame = Vec::new();
        let mut offset_in_destination: Offset = Offset::Added(0);
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);

        let (hunk, change) = process_change(
            &mut lines_blamed,
            &mut new_hunks_to_blame,
            &mut offset_in_destination,
            suspect,
            // range_in_destination: 0..4
            Some(new_unblamed_hunk(1..5, suspect, Offset::Added(1))),
            Some(Change::Added(0..3, 1)),
        );

        assert_eq!(
            hunk,
            Some(UnblamedHunk {
                range_in_blamed_file: 4..5,
                suspects: [(suspect, 3..4)].into()
            })
        );
        assert_eq!(change, None);
        assert_eq!(
            lines_blamed,
            [BlameEntry {
                range_in_blamed_file: 1..4,
                range_in_original_file: 0..3,
                commit_id: suspect
            }]
        );
        assert_eq!(new_hunks_to_blame, []);
        assert_eq!(offset_in_destination, Offset::Added(2));
    }

    #[test]
    fn added_hunk_7() {
        let mut lines_blamed = Vec::new();
        let mut new_hunks_to_blame = Vec::new();
        let mut offset_in_destination: Offset = Offset::Added(2);
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);

        let (hunk, change) = process_change(
            &mut lines_blamed,
            &mut new_hunks_to_blame,
            &mut offset_in_destination,
            suspect,
            // range_in_destination: 2..6
            Some(new_unblamed_hunk(3..7, suspect, Offset::Added(1))),
            Some(Change::Added(3..5, 1)),
        );

        assert_eq!(
            hunk,
            Some(UnblamedHunk {
                range_in_blamed_file: 6..7,
                suspects: [(suspect, 5..6)].into()
            })
        );
        assert_eq!(change, None);
        assert_eq!(
            lines_blamed,
            [BlameEntry {
                range_in_blamed_file: 4..6,
                range_in_original_file: 3..5,
                commit_id: suspect
            }]
        );
        assert_eq!(
            new_hunks_to_blame,
            [UnblamedHunk {
                range_in_blamed_file: 3..4,
                suspects: [(suspect, 0..1)].into()
            }]
        );
        assert_eq!(offset_in_destination, Offset::Added(3));
    }

    #[test]
    fn added_hunk_8() {
        let mut lines_blamed = Vec::new();
        let mut new_hunks_to_blame = Vec::new();
        let mut offset_in_destination: Offset = Offset::Added(1);
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);

        let (hunk, change) = process_change(
            &mut lines_blamed,
            &mut new_hunks_to_blame,
            &mut offset_in_destination,
            suspect,
            // range_in_destination: 25..26
            Some(new_unblamed_hunk(23..24, suspect, Offset::Deleted(2))),
            Some(Change::Added(25..27, 1)),
        );

        assert_eq!(hunk, None);
        assert_eq!(change, Some(Change::Added(25..27, 1)));
        assert_eq!(
            lines_blamed,
            [BlameEntry {
                range_in_blamed_file: 23..24,
                range_in_original_file: 25..26,
                commit_id: suspect
            }]
        );
        assert_eq!(new_hunks_to_blame, []);
        assert_eq!(offset_in_destination, Offset::Added(1));
    }

    #[test]
    fn added_hunk_9() {
        let mut lines_blamed = Vec::new();
        let mut new_hunks_to_blame = Vec::new();
        let mut offset_in_destination: Offset = Offset::Added(0);
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);

        let (hunk, change) = process_change(
            &mut lines_blamed,
            &mut new_hunks_to_blame,
            &mut offset_in_destination,
            suspect,
            // range_in_destination: 21..22
            Some(new_unblamed_hunk(23..24, suspect, Offset::Added(2))),
            Some(Change::Added(18..22, 3)),
        );

        assert_eq!(hunk, None);
        assert_eq!(change, None);
        assert_eq!(
            lines_blamed,
            [BlameEntry {
                range_in_blamed_file: 23..24,
                range_in_original_file: 21..22,
                commit_id: suspect
            }]
        );
        assert_eq!(new_hunks_to_blame, []);
        assert_eq!(offset_in_destination, Offset::Added(1));
    }

    #[test]
    fn added_hunk_10() {
        let mut lines_blamed = Vec::new();
        let mut new_hunks_to_blame = Vec::new();
        let mut offset_in_destination: Offset = Offset::Added(0);
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);

        let (hunk, change) = process_change(
            &mut lines_blamed,
            &mut new_hunks_to_blame,
            &mut offset_in_destination,
            suspect,
            // range_in_destination: 70..108
            Some(new_unblamed_hunk(71..109, suspect, Offset::Added(1))),
            Some(Change::Added(106..109, 0)),
        );

        assert_eq!(hunk, None);
        assert_eq!(change, Some(Change::Added(106..109, 0)));
        assert_eq!(
            lines_blamed,
            [BlameEntry {
                range_in_blamed_file: 107..109,
                range_in_original_file: 106..108,
                commit_id: suspect
            }]
        );
        assert_eq!(
            new_hunks_to_blame,
            [UnblamedHunk {
                range_in_blamed_file: 71..107,
                suspects: [(suspect, 70..106)].into()
            }]
        );
        assert_eq!(offset_in_destination, Offset::Added(0));
    }

    #[test]
    fn added_hunk_11() {
        let mut lines_blamed = Vec::new();
        let mut new_hunks_to_blame = Vec::new();
        let mut offset_in_destination: Offset = Offset::Added(0);
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);

        let (hunk, change) = process_change(
            &mut lines_blamed,
            &mut new_hunks_to_blame,
            &mut offset_in_destination,
            suspect,
            // range_in_destination: 137..144
            Some(new_unblamed_hunk(149..156, suspect, Offset::Added(12))),
            Some(Change::Added(143..146, 0)),
        );

        assert_eq!(hunk, None);
        assert_eq!(change, Some(Change::Added(143..146, 0)));
        assert_eq!(
            lines_blamed,
            [BlameEntry {
                range_in_blamed_file: 155..156,
                range_in_original_file: 143..144,
                commit_id: suspect
            }]
        );
        assert_eq!(
            new_hunks_to_blame,
            [UnblamedHunk {
                range_in_blamed_file: 149..155,
                suspects: [(suspect, 137..143)].into()
            }]
        );
        assert_eq!(offset_in_destination, Offset::Added(0));
    }

    #[test]
    fn no_overlap() {
        let mut lines_blamed = Vec::new();
        let mut new_hunks_to_blame = Vec::new();
        let mut offset_in_destination: Offset = Offset::Deleted(3);
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);

        let (hunk, change) = process_change(
            &mut lines_blamed,
            &mut new_hunks_to_blame,
            &mut offset_in_destination,
            suspect,
            // range_in_destination: 2..5
            Some(new_unblamed_hunk(3..6, suspect, Offset::Added(1))),
            Some(Change::Added(7..10, 1)),
        );

        assert_eq!(hunk, None);
        assert_eq!(change, Some(Change::Added(7..10, 1)));
        assert_eq!(lines_blamed, []);
        assert_eq!(
            new_hunks_to_blame,
            [UnblamedHunk {
                range_in_blamed_file: 3..6,
                suspects: [(suspect, 5..8)].into()
            }]
        );
        assert_eq!(offset_in_destination, Offset::Deleted(3));
    }

    #[test]
    fn no_overlap_2() {
        let mut lines_blamed = Vec::new();
        let mut new_hunks_to_blame = Vec::new();
        let mut offset_in_destination: Offset = Offset::Added(0);
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);

        let (hunk, change) = process_change(
            &mut lines_blamed,
            &mut new_hunks_to_blame,
            &mut offset_in_destination,
            suspect,
            // range_in_destination: 6..8
            Some(new_unblamed_hunk(9..11, suspect, Offset::Added(3))),
            Some(Change::Added(2..5, 0)),
        );

        assert_eq!(
            hunk,
            Some(UnblamedHunk {
                range_in_blamed_file: 9..11,
                suspects: [(suspect, 6..8)].into()
            })
        );
        assert_eq!(change, None);
        assert_eq!(lines_blamed, []);
        assert_eq!(new_hunks_to_blame, []);
        assert_eq!(offset_in_destination, Offset::Added(3));
    }

    #[test]
    fn no_overlap_3() {
        let mut lines_blamed = Vec::new();
        let mut new_hunks_to_blame = Vec::new();
        let mut offset_in_destination: Offset = Offset::Added(0);
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);

        let (hunk, change) = process_change(
            &mut lines_blamed,
            &mut new_hunks_to_blame,
            &mut offset_in_destination,
            suspect,
            // range_in_destination: 5..15
            Some(new_unblamed_hunk(4..15, suspect, Offset::Deleted(1))),
            Some(Change::Added(4..5, 1)),
        );

        assert_eq!(
            hunk,
            Some(UnblamedHunk {
                range_in_blamed_file: 4..15,
                suspects: [(suspect, 5..16)].into()
            })
        );
        assert_eq!(change, None);
        assert_eq!(lines_blamed, []);
        assert_eq!(new_hunks_to_blame, []);
        assert_eq!(offset_in_destination, Offset::Added(0));
    }

    #[test]
    fn no_overlap_4() {
        let mut lines_blamed = Vec::new();
        let mut new_hunks_to_blame = Vec::new();
        let mut offset_in_destination: Offset = Offset::Added(1);
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);

        let (hunk, change) = process_change(
            &mut lines_blamed,
            &mut new_hunks_to_blame,
            &mut offset_in_destination,
            suspect,
            // range_in_destination: 25..27
            Some(new_unblamed_hunk(23..25, suspect, Offset::Deleted(2))),
            Some(Change::Unchanged(21..22)),
        );

        assert_eq!(
            hunk,
            Some(UnblamedHunk {
                range_in_blamed_file: 23..25,
                suspects: [(suspect, 25..27)].into()
            })
        );
        assert_eq!(change, None);
        assert_eq!(lines_blamed, []);
        assert_eq!(new_hunks_to_blame, []);
        assert_eq!(offset_in_destination, Offset::Added(1));
    }

    #[test]
    fn no_overlap_5() {
        let mut lines_blamed = Vec::new();
        let mut new_hunks_to_blame = Vec::new();
        let mut offset_in_destination: Offset = Offset::Added(1);
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);

        let (hunk, change) = process_change(
            &mut lines_blamed,
            &mut new_hunks_to_blame,
            &mut offset_in_destination,
            suspect,
            // range_in_destination: 17..18
            Some(new_unblamed_hunk(15..16, suspect, Offset::Deleted(2))),
            Some(Change::Deleted(20, 1)),
        );

        assert_eq!(hunk, None);
        assert_eq!(change, Some(Change::Deleted(20, 1)));
        assert_eq!(lines_blamed, []);
        assert_eq!(
            new_hunks_to_blame,
            [UnblamedHunk {
                range_in_blamed_file: 15..16,
                suspects: [(suspect, 16..17)].into()
            }]
        );
        assert_eq!(offset_in_destination, Offset::Added(1));
    }

    #[test]
    fn no_overlap_6() {
        let mut lines_blamed = Vec::new();
        let mut new_hunks_to_blame = Vec::new();
        let mut offset_in_destination: Offset = Offset::Added(0);
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);

        let (hunk, change) = process_change(
            &mut lines_blamed,
            &mut new_hunks_to_blame,
            &mut offset_in_destination,
            suspect,
            // range_in_destination: 22..24
            Some(new_unblamed_hunk(23..25, suspect, Offset::Added(1))),
            Some(Change::Deleted(20, 1)),
        );

        assert_eq!(
            hunk,
            Some(UnblamedHunk {
                range_in_blamed_file: 23..25,
                suspects: [(suspect, 22..24)].into()
            })
        );
        assert_eq!(change, None);
        assert_eq!(lines_blamed, []);
        assert_eq!(new_hunks_to_blame, []);
        assert_eq!(offset_in_destination, Offset::Deleted(1));
    }

    #[test]
    fn enclosing_addition() {
        let mut lines_blamed = Vec::new();
        let mut new_hunks_to_blame = Vec::new();
        let mut offset_in_destination: Offset = Offset::Added(3);
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);

        let (hunk, change) = process_change(
            &mut lines_blamed,
            &mut new_hunks_to_blame,
            &mut offset_in_destination,
            suspect,
            // range_in_destination: 5..8
            Some(new_unblamed_hunk(2..5, suspect, Offset::Deleted(3))),
            Some(Change::Added(3..12, 2)),
        );

        assert_eq!(hunk, None);
        assert_eq!(change, Some(Change::Added(3..12, 2)));
        assert_eq!(
            lines_blamed,
            [BlameEntry {
                range_in_blamed_file: 2..5,
                range_in_original_file: 5..8,
                commit_id: suspect
            }]
        );
        assert_eq!(new_hunks_to_blame, []);
        assert_eq!(offset_in_destination, Offset::Added(3));
    }

    #[test]
    fn enclosing_deletion() {
        let mut lines_blamed = Vec::new();
        let mut new_hunks_to_blame = Vec::new();
        let mut offset_in_destination: Offset = Offset::Added(3);
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);

        let (hunk, change) = process_change(
            &mut lines_blamed,
            &mut new_hunks_to_blame,
            &mut offset_in_destination,
            suspect,
            // range_in_destination: 13..20
            Some(new_unblamed_hunk(12..19, suspect, Offset::Deleted(1))),
            Some(Change::Deleted(15, 2)),
        );

        assert_eq!(
            hunk,
            Some(UnblamedHunk {
                range_in_blamed_file: 14..19,
                suspects: [(suspect, 15..20)].into()
            })
        );
        assert_eq!(change, None);
        assert_eq!(lines_blamed, []);
        assert_eq!(
            new_hunks_to_blame,
            [UnblamedHunk {
                range_in_blamed_file: 12..14,
                suspects: [(suspect, 10..12)].into()
            }]
        );
        assert_eq!(offset_in_destination, Offset::Added(1));
    }

    #[test]
    fn enclosing_unchanged_lines() {
        let mut lines_blamed = Vec::new();
        let mut new_hunks_to_blame = Vec::new();
        let mut offset_in_destination: Offset = Offset::Added(3);
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);

        let (hunk, change) = process_change(
            &mut lines_blamed,
            &mut new_hunks_to_blame,
            &mut offset_in_destination,
            suspect,
            // range_in_destination: 109..113
            Some(new_unblamed_hunk(110..114, suspect, Offset::Added(1))),
            Some(Change::Unchanged(109..172)),
        );

        assert_eq!(hunk, None);
        assert_eq!(change, Some(Change::Unchanged(109..172)));
        assert_eq!(lines_blamed, []);
        assert_eq!(
            new_hunks_to_blame,
            [UnblamedHunk {
                range_in_blamed_file: 110..114,
                suspects: [(suspect, 106..110)].into()
            }]
        );
        assert_eq!(offset_in_destination, Offset::Added(3));
    }

    #[test]
    fn unchanged_hunk() {
        let mut lines_blamed = Vec::new();
        let mut new_hunks_to_blame = Vec::new();
        let mut offset_in_destination: Offset = Offset::Added(0);
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);

        let (hunk, change) = process_change(
            &mut lines_blamed,
            &mut new_hunks_to_blame,
            &mut offset_in_destination,
            suspect,
            Some(new_unblamed_hunk(0..5, suspect, Offset::Added(0))),
            Some(Change::Unchanged(0..3)),
        );

        assert_eq!(
            hunk,
            Some(UnblamedHunk {
                range_in_blamed_file: 0..5,
                suspects: [(suspect, 0..5)].into()
            })
        );
        assert_eq!(change, None);
        assert_eq!(lines_blamed, []);
        assert_eq!(new_hunks_to_blame, []);
        assert_eq!(offset_in_destination, Offset::Added(0));
    }

    #[test]
    fn unchanged_hunk_2() {
        let mut lines_blamed = Vec::new();
        let mut new_hunks_to_blame = Vec::new();
        let mut offset_in_destination: Offset = Offset::Added(0);
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);

        let (hunk, change) = process_change(
            &mut lines_blamed,
            &mut new_hunks_to_blame,
            &mut offset_in_destination,
            suspect,
            Some(new_unblamed_hunk(0..5, suspect, Offset::Added(0))),
            Some(Change::Unchanged(0..7)),
        );

        assert_eq!(hunk, None);
        assert_eq!(change, Some(Change::Unchanged(0..7)));
        assert_eq!(lines_blamed, []);
        assert_eq!(
            new_hunks_to_blame,
            [UnblamedHunk {
                range_in_blamed_file: 0..5,
                suspects: [(suspect, 0..5)].into()
            }]
        );
        assert_eq!(offset_in_destination, Offset::Added(0));
    }

    #[test]
    fn unchanged_hunk_3() {
        let mut lines_blamed = Vec::new();
        let mut new_hunks_to_blame = Vec::new();
        let mut offset_in_destination: Offset = Offset::Deleted(2);
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);

        let (hunk, change) = process_change(
            &mut lines_blamed,
            &mut new_hunks_to_blame,
            &mut offset_in_destination,
            suspect,
            Some(UnblamedHunk {
                range_in_blamed_file: 22..30,
                suspects: [(suspect, 21..29)].into(),
            }),
            Some(Change::Unchanged(21..23)),
        );

        assert_eq!(
            hunk,
            Some(UnblamedHunk {
                range_in_blamed_file: 22..30,
                suspects: [(suspect, 21..29)].into()
            })
        );
        assert_eq!(change, None);
        assert_eq!(lines_blamed, []);
        assert_eq!(new_hunks_to_blame, []);
        assert_eq!(offset_in_destination, Offset::Deleted(2));
    }

    #[test]
    fn deleted_hunk() {
        let mut lines_blamed = Vec::new();
        let mut new_hunks_to_blame = Vec::new();
        let mut offset_in_destination: Offset = Offset::Added(0);
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);

        let (hunk, change) = process_change(
            &mut lines_blamed,
            &mut new_hunks_to_blame,
            &mut offset_in_destination,
            suspect,
            Some(new_unblamed_hunk(0..5, suspect, Offset::Added(0))),
            Some(Change::Deleted(5, 3)),
        );

        assert_eq!(hunk, None);
        assert_eq!(change, Some(Change::Deleted(5, 3)));
        assert_eq!(lines_blamed, []);
        assert_eq!(
            new_hunks_to_blame,
            [UnblamedHunk {
                range_in_blamed_file: 0..5,
                suspects: [(suspect, 0..5)].into()
            }]
        );
        assert_eq!(offset_in_destination, Offset::Added(0));
    }

    #[test]
    fn deleted_hunk_2() {
        let mut lines_blamed = Vec::new();
        let mut new_hunks_to_blame = Vec::new();
        let mut offset_in_destination: Offset = Offset::Added(0);
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);

        let (hunk, change) = process_change(
            &mut lines_blamed,
            &mut new_hunks_to_blame,
            &mut offset_in_destination,
            suspect,
            Some(new_unblamed_hunk(2..16, suspect, Offset::Added(0))),
            Some(Change::Deleted(0, 4)),
        );

        assert_eq!(
            hunk,
            Some(UnblamedHunk {
                range_in_blamed_file: 2..16,
                suspects: [(suspect, 2..16)].into()
            })
        );
        assert_eq!(change, None);
        assert_eq!(lines_blamed, []);
        assert_eq!(new_hunks_to_blame, []);
        assert_eq!(offset_in_destination, Offset::Deleted(4));
    }

    #[test]
    fn deleted_hunk_3() {
        let mut lines_blamed = Vec::new();
        let mut new_hunks_to_blame = Vec::new();
        let mut offset_in_destination: Offset = Offset::Added(0);
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);

        let (hunk, change) = process_change(
            &mut lines_blamed,
            &mut new_hunks_to_blame,
            &mut offset_in_destination,
            suspect,
            Some(new_unblamed_hunk(2..16, suspect, Offset::Added(0))),
            Some(Change::Deleted(14, 4)),
        );

        assert_eq!(
            hunk,
            Some(UnblamedHunk {
                range_in_blamed_file: 14..16,
                suspects: [(suspect, 14..16)].into()
            })
        );
        assert_eq!(change, None);
        assert_eq!(lines_blamed, []);
        assert_eq!(
            new_hunks_to_blame,
            [new_unblamed_hunk(2..14, suspect, Offset::Added(0))]
        );
        assert_eq!(offset_in_destination, Offset::Deleted(4));
    }

    #[test]
    fn addition_only() {
        let mut lines_blamed = Vec::new();
        let mut new_hunks_to_blame = Vec::new();
        let mut offset_in_destination: Offset = Offset::Added(1);
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);

        let (hunk, change) = process_change(
            &mut lines_blamed,
            &mut new_hunks_to_blame,
            &mut offset_in_destination,
            suspect,
            None,
            Some(Change::Added(22..25, 1)),
        );

        assert_eq!(hunk, None);
        assert_eq!(change, None);
        assert_eq!(lines_blamed, []);
        assert_eq!(new_hunks_to_blame, []);
        assert_eq!(offset_in_destination, Offset::Added(3));
    }

    #[test]
    fn deletion_only() {
        let mut lines_blamed = Vec::new();
        let mut new_hunks_to_blame = Vec::new();
        let mut offset_in_destination: Offset = Offset::Added(1);
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);

        let (hunk, change) = process_change(
            &mut lines_blamed,
            &mut new_hunks_to_blame,
            &mut offset_in_destination,
            suspect,
            None,
            Some(Change::Deleted(11, 5)),
        );

        assert_eq!(hunk, None);
        assert_eq!(change, None);
        assert_eq!(lines_blamed, []);
        assert_eq!(new_hunks_to_blame, []);
        assert_eq!(offset_in_destination, Offset::Deleted(4));
    }

    #[test]
    fn unchanged_only() {
        let mut lines_blamed = Vec::new();
        let mut new_hunks_to_blame = Vec::new();
        let mut offset_in_destination: Offset = Offset::Added(1);
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);

        let (hunk, change) = process_change(
            &mut lines_blamed,
            &mut new_hunks_to_blame,
            &mut offset_in_destination,
            suspect,
            None,
            Some(Change::Unchanged(11..13)),
        );

        assert_eq!(hunk, None);
        assert_eq!(change, None);
        assert_eq!(lines_blamed, []);
        assert_eq!(new_hunks_to_blame, []);
        assert_eq!(offset_in_destination, Offset::Added(1));
    }
}
mod process_changes {
    use crate::file::tests::new_unblamed_hunk;
    use crate::file::{process_changes, Change, Offset, UnblamedHunk};
    use crate::BlameEntry;
    use gix_hash::ObjectId;

    #[test]
    fn nothing() {
        let mut lines_blamed = Vec::new();
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);
        let new_hunks_to_blame = process_changes(&mut lines_blamed, vec![], vec![], suspect);

        assert_eq!(lines_blamed, []);
        assert_eq!(new_hunks_to_blame, []);
    }

    #[test]
    fn added_hunk() {
        let mut lines_blamed = Vec::new();
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);
        let hunks_to_blame = vec![new_unblamed_hunk(0..4, suspect, Offset::Added(0))];
        let changes = vec![Change::Added(0..4, 0)];
        let new_hunks_to_blame = process_changes(&mut lines_blamed, hunks_to_blame, changes, suspect);

        assert_eq!(
            lines_blamed,
            [BlameEntry {
                range_in_blamed_file: 0..4,
                range_in_original_file: 0..4,
                commit_id: suspect
            }]
        );
        assert_eq!(new_hunks_to_blame, []);
    }

    #[test]
    fn added_hunk_2() {
        let mut lines_blamed = Vec::new();
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);
        let hunks_to_blame = vec![new_unblamed_hunk(0..6, suspect, Offset::Added(0))];
        let changes = vec![Change::Added(0..4, 0), Change::Unchanged(4..6)];
        let new_hunks_to_blame = process_changes(&mut lines_blamed, hunks_to_blame, changes, suspect);

        assert_eq!(
            lines_blamed,
            [BlameEntry {
                range_in_blamed_file: 0..4,
                range_in_original_file: 0..4,
                commit_id: suspect
            }]
        );
        assert_eq!(new_hunks_to_blame, [new_unblamed_hunk(4..6, suspect, Offset::Added(4))]);
    }

    #[test]
    fn added_hunk_3() {
        let mut lines_blamed = Vec::new();
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);
        let hunks_to_blame = vec![new_unblamed_hunk(0..6, suspect, Offset::Added(0))];
        let changes = vec![Change::Unchanged(0..2), Change::Added(2..4, 0), Change::Unchanged(4..6)];
        let new_hunks_to_blame = process_changes(&mut lines_blamed, hunks_to_blame, changes, suspect);

        assert_eq!(
            lines_blamed,
            [BlameEntry {
                range_in_blamed_file: 2..4,
                range_in_original_file: 2..4,
                commit_id: suspect
            }]
        );
        assert_eq!(
            new_hunks_to_blame,
            [
                new_unblamed_hunk(0..2, suspect, Offset::Added(0)),
                new_unblamed_hunk(4..6, suspect, Offset::Added(2))
            ]
        );
    }

    #[test]
    fn added_hunk_4_0() {
        let mut lines_blamed = Vec::new();
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);
        let hunks_to_blame = vec![new_unblamed_hunk(0..6, suspect, Offset::Added(0))];
        let changes = vec![Change::Added(0..1, 0), Change::Added(1..4, 0), Change::Unchanged(4..6)];
        let new_hunks_to_blame = process_changes(&mut lines_blamed, hunks_to_blame, changes, suspect);

        assert_eq!(
            lines_blamed,
            [
                BlameEntry {
                    range_in_blamed_file: 0..1,
                    range_in_original_file: 0..1,
                    commit_id: suspect
                },
                BlameEntry {
                    range_in_blamed_file: 1..4,
                    range_in_original_file: 1..4,
                    commit_id: suspect
                }
            ]
        );
        assert_eq!(new_hunks_to_blame, [new_unblamed_hunk(4..6, suspect, Offset::Added(4))]);
    }

    #[test]
    fn added_hunk_4_1() {
        let mut lines_blamed = Vec::new();
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);
        let hunks_to_blame = vec![new_unblamed_hunk(0..6, suspect, Offset::Added(0))];
        let changes = vec![Change::Added(0..1, 0)];
        let new_hunks_to_blame = process_changes(&mut lines_blamed, hunks_to_blame, changes, suspect);

        assert_eq!(
            lines_blamed,
            [BlameEntry {
                range_in_blamed_file: 0..1,
                range_in_original_file: 0..1,
                commit_id: suspect
            }]
        );
        assert_eq!(new_hunks_to_blame, [new_unblamed_hunk(1..6, suspect, Offset::Added(1))]);
    }

    #[test]
    fn added_hunk_4_2() {
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);
        let suspect_2 = ObjectId::from_hex(b"2222222222222222222222222222222222222222").unwrap();
        let mut lines_blamed: Vec<BlameEntry> = vec![BlameEntry {
            range_in_blamed_file: 0..2,
            range_in_original_file: 0..2,
            commit_id: suspect,
        }];
        let hunks_to_blame = vec![new_unblamed_hunk(2..6, suspect_2, Offset::Added(2))];
        let changes = vec![Change::Added(0..1, 0)];
        let new_hunks_to_blame = process_changes(&mut lines_blamed, hunks_to_blame, changes, suspect_2);

        assert_eq!(
            lines_blamed,
            [
                BlameEntry {
                    range_in_blamed_file: 0..2,
                    range_in_original_file: 0..2,
                    commit_id: suspect
                },
                BlameEntry {
                    range_in_blamed_file: 2..3,
                    range_in_original_file: 0..1,
                    commit_id: suspect_2
                }
            ]
        );
        assert_eq!(
            new_hunks_to_blame,
            [new_unblamed_hunk(3..6, suspect_2, Offset::Added(3))]
        );
    }

    #[test]
    fn added_hunk_5() {
        let mut lines_blamed = Vec::new();
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);
        let hunks_to_blame = vec![new_unblamed_hunk(0..6, suspect, Offset::Added(0))];
        let changes = vec![Change::Added(0..4, 3), Change::Unchanged(4..6)];
        let new_hunks_to_blame = process_changes(&mut lines_blamed, hunks_to_blame, changes, suspect);

        assert_eq!(
            lines_blamed,
            [BlameEntry {
                range_in_blamed_file: 0..4,
                range_in_original_file: 0..4,
                commit_id: suspect
            }]
        );
        assert_eq!(new_hunks_to_blame, [new_unblamed_hunk(4..6, suspect, Offset::Added(1))]);
    }

    #[test]
    fn added_hunk_6() {
        let mut lines_blamed = Vec::new();
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);
        let hunks_to_blame = vec![new_unblamed_hunk(4..6, suspect, Offset::Added(1))];
        let changes = vec![Change::Added(0..3, 0), Change::Unchanged(3..5)];
        let new_hunks_to_blame = process_changes(&mut lines_blamed, hunks_to_blame, changes, suspect);

        assert_eq!(lines_blamed, []);
        assert_eq!(new_hunks_to_blame, [new_unblamed_hunk(4..6, suspect, Offset::Added(4))]);
    }

    #[test]
    fn added_hunk_7() {
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);
        let suspect_2 = ObjectId::from_hex(b"2222222222222222222222222222222222222222").unwrap();
        let mut lines_blamed: Vec<BlameEntry> = vec![BlameEntry {
            range_in_blamed_file: 0..1,
            range_in_original_file: 0..1,
            commit_id: suspect,
        }];
        let hunks_to_blame = vec![new_unblamed_hunk(1..3, suspect_2, Offset::Added(1))];
        let changes = vec![Change::Added(0..1, 2)];
        let new_hunks_to_blame = process_changes(&mut lines_blamed, hunks_to_blame, changes, suspect_2);

        assert_eq!(
            lines_blamed,
            [
                BlameEntry {
                    range_in_blamed_file: 0..1,
                    range_in_original_file: 0..1,
                    commit_id: suspect
                },
                BlameEntry {
                    range_in_blamed_file: 1..2,
                    range_in_original_file: 0..1,
                    commit_id: suspect_2
                }
            ]
        );
        assert_eq!(
            new_hunks_to_blame,
            [new_unblamed_hunk(2..3, suspect_2, Offset::Added(0))]
        );
    }

    #[test]
    fn added_hunk_8() {
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);
        let mut lines_blamed = Vec::new();
        let hunks_to_blame = vec![new_unblamed_hunk(0..4, suspect, Offset::Added(0))];
        let changes = vec![Change::Added(0..2, 0), Change::Unchanged(2..3), Change::Added(3..4, 0)];
        let new_hunks_to_blame = process_changes(&mut lines_blamed, hunks_to_blame, changes, suspect);

        assert_eq!(
            lines_blamed,
            [
                BlameEntry {
                    range_in_blamed_file: 0..2,
                    range_in_original_file: 0..2,
                    commit_id: suspect
                },
                BlameEntry {
                    range_in_blamed_file: 3..4,
                    range_in_original_file: 3..4,
                    commit_id: suspect
                }
            ]
        );
        assert_eq!(new_hunks_to_blame, [new_unblamed_hunk(2..3, suspect, Offset::Added(2))]);
    }

    #[test]
    fn added_hunk_9() {
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);
        let mut lines_blamed: Vec<BlameEntry> = vec![BlameEntry {
            range_in_blamed_file: 30..31,
            range_in_original_file: 30..31,
            commit_id: suspect,
        }];
        let hunks_to_blame = vec![
            UnblamedHunk {
                range_in_blamed_file: 0..30,
                suspects: [(suspect, 0..30)].into(),
            },
            UnblamedHunk {
                range_in_blamed_file: 31..37,
                suspects: [(suspect, 31..37)].into(),
            },
        ];
        let changes = vec![
            Change::Unchanged(0..16),
            Change::Added(16..17, 0),
            Change::Unchanged(17..37),
        ];
        let new_hunks_to_blame = process_changes(&mut lines_blamed, hunks_to_blame, changes, suspect);

        lines_blamed.sort_by(|a, b| a.range_in_blamed_file.start.cmp(&b.range_in_blamed_file.start));

        assert_eq!(
            lines_blamed,
            [
                BlameEntry {
                    range_in_blamed_file: 16..17,
                    range_in_original_file: 16..17,
                    commit_id: suspect
                },
                BlameEntry {
                    range_in_blamed_file: 30..31,
                    range_in_original_file: 30..31,
                    commit_id: suspect
                }
            ]
        );
        assert_eq!(
            new_hunks_to_blame,
            [
                UnblamedHunk {
                    range_in_blamed_file: 0..16,
                    suspects: [(suspect, 0..16)].into()
                },
                UnblamedHunk {
                    range_in_blamed_file: 17..30,
                    suspects: [(suspect, 16..29)].into()
                },
                UnblamedHunk {
                    range_in_blamed_file: 31..37,
                    suspects: [(suspect, 30..36)].into()
                }
            ]
        );
    }

    #[test]
    fn deleted_hunk() {
        let mut lines_blamed = Vec::new();
        let suspect = ObjectId::null(gix_hash::Kind::Sha1);
        let hunks_to_blame = vec![
            new_unblamed_hunk(0..4, suspect, Offset::Added(0)),
            new_unblamed_hunk(4..7, suspect, Offset::Added(0)),
        ];
        let changes = vec![Change::Deleted(0, 3), Change::Added(0..4, 0)];
        let new_hunks_to_blame = process_changes(&mut lines_blamed, hunks_to_blame, changes, suspect);

        assert_eq!(
            lines_blamed,
            [BlameEntry {
                range_in_blamed_file: 0..4,
                range_in_original_file: 0..4,
                commit_id: suspect
            }]
        );
        assert_eq!(
            new_hunks_to_blame,
            [UnblamedHunk {
                range_in_blamed_file: 4..7,
                suspects: [(suspect, 3..6)].into()
            }]
        );
    }
}
