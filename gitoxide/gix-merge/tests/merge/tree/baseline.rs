use bstr::{BStr, ByteSlice};
use gix_hash::ObjectId;
use gix_merge::blob::builtin_driver::text::ConflictStyle;
use gix_object::tree::EntryMode;
use gix_object::FindExt;
use std::path::{Path, PathBuf};

/// An entry in the conflict
#[derive(Debug)]
pub struct Entry {
    /// The relative path in the repository
    pub location: String,
    /// The content id.
    pub id: gix_hash::ObjectId,
    /// The kind of entry.
    pub mode: EntryMode,
}

/// Keep track of all the sides of a conflict. Some might not be set to indicate removal, including the ancestor.
#[derive(Default, Debug)]
pub struct Conflict {
    pub ancestor: Option<Entry>,
    pub ours: Option<Entry>,
    pub theirs: Option<Entry>,
}

#[derive(Debug)]
pub enum ConflictKind {
    /// The conflict was resolved by automatically merging the content.
    AutoMerging,
    /// The content could not be resolved so it's conflicting.
    ConflictContents,
    /// Directory in theirs in the way of our file.
    ConflictDirectoryBlocksFile,
    /// Modified in ours but deleted in theirs.
    ConflictModifyDelete,
    /// Modified in ours but parent directory renamed in theirs.
    DirectoryRenamedWithModificationInside,
    /// Added files differ in mode.
    DistinctModes,
    /// The same file was renamed to different destinations.
    RenameRename,
    /// Deleted in ours with a new file added, renamed to new file in theirs with original content.
    RenameAddDelete,
    /// Two binary files were changed in different ways, which can never be merged (without a merge-driver)
    Binary,
}

/// More loosely structured information about the `Conflict`.
#[derive(Debug)]
pub struct ConflictInfo {
    /// All the paths involved in the informational message
    pub paths: Vec<String>,
    /// The type of the conflict, further described in `message`.
    pub kind: ConflictKind,
    /// An arbitrary message formed from paths and kind
    pub message: String,
}

impl Conflict {
    fn any_location(&self) -> Option<&str> {
        self.ancestor
            .as_ref()
            .or(self.ours.as_ref())
            .or(self.theirs.as_ref())
            .map(|a| a.location.as_str())
    }
    fn storage_for(&mut self, side: Side, location: &str) -> Option<&mut Option<Entry>> {
        let current_location = self.any_location();
        let location_is_same = current_location.is_none() || current_location == Some(location);
        let side = match side {
            Side::Ancestor => &mut self.ancestor,
            Side::Ours => &mut self.ours,
            Side::Theirs => &mut self.theirs,
        };
        (!side.is_some() && location_is_same).then_some(side)
    }
}

pub struct MergeInfo {
    /// The hash of the merged tree - it may contain intermediate files if the merge didn't succeed entirely.
    pub merged_tree: gix_hash::ObjectId,
    /// If there were conflicts, this is the conflicting paths.
    pub conflicts: Option<Vec<Conflict>>,
    /// Structured details which to some extent can be compared to our own conflict information.
    pub information: Vec<ConflictInfo>,
}

pub struct Expectation {
    pub root: PathBuf,
    pub conflict_style: gix_merge::blob::builtin_driver::text::ConflictStyle,
    pub odb: gix_odb::memory::Proxy<gix_odb::Handle>,
    pub our_commit_id: gix_hash::ObjectId,
    pub our_side_name: String,
    pub their_commit_id: gix_hash::ObjectId,
    pub their_side_name: String,
    pub merge_info: MergeInfo,
    pub case_name: String,
    pub deviation: Option<Deviation>,
}

/// Git doesn't provide the same result.
pub struct Deviation {
    /// Tells us the reason for expecting a difference compared to the Git result.
    pub message: String,
    /// The tree we wish to see, it's hand-crafted directly in the test as Git can't provide the baseline here.
    pub expected_tree_id: gix_hash::ObjectId,
}

pub struct Expectations<'a> {
    root: &'a Path,
    lines: std::str::Lines<'a>,
}

impl<'a> Expectations<'a> {
    pub fn new(root: &'a Path, cases: &'a str) -> Self {
        Expectations {
            root,
            lines: cases.lines(),
        }
    }
}

impl Iterator for Expectations<'_> {
    type Item = Expectation;

    fn next(&mut self) -> Option<Self::Item> {
        let line = self.lines.next()?;
        let mut tokens = line.split(' ');
        let (
            Some(subdir),
            Some(conflict_style),
            Some(our_commit_id),
            Some(our_side_name),
            Some(their_commit_id),
            Some(their_side_name),
            Some(merge_info_filename),
            Some(expected_custom_tree),
        ) = (
            tokens.next(),
            tokens.next(),
            tokens.next(),
            tokens.next(),
            tokens.next(),
            tokens.next(),
            tokens.next(),
            tokens.next(),
        )
        else {
            unreachable!("invalid line: {line:?}")
        };
        let deviation = (expected_custom_tree != "expected^{tree}").then(|| {
            let expected_tree_id = gix_hash::ObjectId::from_hex(expected_custom_tree.as_bytes())
                .expect("valid tree id in hex for the expected tree");
            let message = tokens.collect::<Vec<_>>().join(" ").trim().to_owned();
            Deviation {
                message,
                expected_tree_id,
            }
        });

        let subdir_path = self.root.join(subdir);
        let conflict_style = match conflict_style {
            "merge" => ConflictStyle::Merge,
            "diff3" => ConflictStyle::Diff3,
            unknown => unreachable!("Unknown conflict style: '{unknown}'"),
        };
        let odb = gix_odb::at(subdir_path.join(".git/objects")).expect("object dir exists");
        let objects = gix_odb::memory::Proxy::new(odb, gix_hash::Kind::Sha1);
        let our_commit_id = gix_hash::ObjectId::from_hex(our_commit_id.as_bytes()).unwrap();
        let their_commit_id = gix_hash::ObjectId::from_hex(their_commit_id.as_bytes()).unwrap();
        let merge_info = parse_merge_info(std::fs::read_to_string(subdir_path.join(merge_info_filename)).unwrap());
        Some(Expectation {
            root: subdir_path,
            conflict_style,
            odb: objects,
            our_commit_id,
            our_side_name: our_side_name.to_owned(),
            their_commit_id,
            their_side_name: their_side_name.to_owned(),
            merge_info,
            case_name: format!(
                "{subdir}-{}",
                merge_info_filename
                    .split('.')
                    .next()
                    .expect("extension after single dot")
            ),
            deviation,
        })
    }
}

fn parse_merge_info(content: String) -> MergeInfo {
    let mut lines = content.split('\0').filter(|t| !t.is_empty()).peekable();
    let tree_id = gix_hash::ObjectId::from_hex(lines.next().unwrap().as_bytes()).unwrap();
    let mut out = MergeInfo {
        merged_tree: tree_id,
        conflicts: None,
        information: Vec::new(),
    };

    let mut conflicts = Vec::new();
    let mut conflict = Conflict::default();
    while let Some(line) = lines.peek() {
        let (entry, side) = match parse_conflict_file_info(line) {
            Some(t) => t,
            None => break,
        };
        lines.next();
        let field = match conflict.storage_for(side, &entry.location) {
            None => {
                conflicts.push(conflict);
                conflict = Conflict::default();
                conflict
                    .storage_for(side, &entry.location)
                    .expect("always available for new side")
            }
            Some(field) => field,
        };
        *field = Some(entry);
    }

    while lines.peek().is_some() {
        out.information
            .push(parse_info(&mut lines).expect("if there are lines, it should be valid info"));
    }
    assert_eq!(lines.next(), None, "TODO: conflict messages");
    out.conflicts = (!conflicts.is_empty()).then_some(conflicts);
    out
}

#[derive(Copy, Clone)]
enum Side {
    Ancestor,
    Ours,
    Theirs,
}

fn parse_conflict_file_info(line: &str) -> Option<(Entry, Side)> {
    let (info, mut path) = line.split_at(line.find('\t')?);
    path = &path[1..];
    let mut tokens = info.split(' ');
    let (oct_mode, hex_id, stage) = (
        tokens.next().expect("mode"),
        tokens.next().expect("id"),
        tokens.next().expect("stage"),
    );
    assert_eq!(
        tokens.next(),
        None,
        "info line not understood, expected three fields only"
    );
    Some((
        Entry {
            location: path.to_owned(),
            id: gix_hash::ObjectId::from_hex(hex_id.as_bytes()).unwrap(),
            mode: EntryMode(gix_utils::btoi::to_signed_with_radix::<usize>(oct_mode.as_bytes(), 8).unwrap() as u16),
        },
        match stage {
            "1" => Side::Ancestor,
            "2" => Side::Ours,
            "3" => Side::Theirs,
            invalid => panic!("{invalid} is an unexpected side"),
        },
    ))
}

fn parse_info<'a>(mut lines: impl Iterator<Item = &'a str>) -> Option<ConflictInfo> {
    let num_paths: usize = lines.next()?.parse().ok()?;
    let paths: Vec<_> = lines.by_ref().take(num_paths).map(ToOwned::to_owned).collect();
    let kind = match lines.next()? {
        "Auto-merging" => ConflictKind::AutoMerging,
        "CONFLICT (contents)" => ConflictKind::ConflictContents,
        "CONFLICT (file/directory)" => ConflictKind::ConflictDirectoryBlocksFile,
        "CONFLICT (modify/delete)" => ConflictKind::ConflictModifyDelete,
        "CONFLICT (directory rename suggested)" => ConflictKind::DirectoryRenamedWithModificationInside,
        "CONFLICT (distinct modes)" => ConflictKind::DistinctModes,
        "CONFLICT (rename/rename)" => ConflictKind::RenameRename,
        "CONFLICT (rename/delete)" => ConflictKind::RenameAddDelete,
        "CONFLICT (binary)" => ConflictKind::Binary,
        conflict_type => panic!("Unkonwn conflict type: {conflict_type}"),
    };
    let message = lines.next()?.to_owned();
    Some(ConflictInfo { paths, kind, message })
}

pub fn visualize_tree(
    id: &gix_hash::oid,
    odb: &impl gix_object::Find,
    name_and_mode: Option<(&BStr, EntryMode)>,
) -> termtree::Tree<String> {
    fn short_id(id: &gix_hash::oid) -> String {
        id.to_string()[..7].to_string()
    }
    let entry_name = |id: &gix_hash::oid, name: Option<(&BStr, EntryMode)>| -> String {
        let mut buf = Vec::new();
        match name {
            None => short_id(id),
            Some((name, mode)) => {
                format!(
                    "{name}:{mode}{} {}",
                    short_id(id),
                    match odb.find_blob(id, &mut buf) {
                        Ok(blob) => format!("{:?}", blob.data.as_bstr()),
                        Err(_) => "".into(),
                    },
                    mode = if mode.is_tree() {
                        "".into()
                    } else {
                        format!("{:o}:", mode.0)
                    }
                )
            }
        }
    };

    let mut tree = termtree::Tree::new(entry_name(id, name_and_mode));
    let mut buf = Vec::new();
    for entry in odb.find_tree(id, &mut buf).unwrap().entries {
        if entry.mode.is_tree() {
            tree.push(visualize_tree(entry.oid, odb, Some((entry.filename, entry.mode))));
        } else {
            tree.push(entry_name(entry.oid, Some((entry.filename, entry.mode))));
        }
    }
    tree
}

pub fn show_diff_and_fail(
    case_name: &str,
    actual_id: ObjectId,
    actual: &gix_merge::tree::Outcome<'_>,
    expected: &MergeInfo,
    odb: &gix_odb::memory::Proxy<gix_odb::Handle>,
) {
    pretty_assertions::assert_str_eq!(
        visualize_tree(&actual_id, odb, None).to_string(),
        visualize_tree(&expected.merged_tree, odb, None).to_string(),
        "{case_name}: merged tree mismatch\n{:#?}\n{:#?}\n{case_name}",
        actual.conflicts,
        expected.information
    );
}
