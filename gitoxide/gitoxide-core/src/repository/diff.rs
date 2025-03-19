use anyhow::Context;
use gix::bstr::{BString, ByteSlice};
use gix::diff::blob::intern::TokenSource;
use gix::diff::blob::unified_diff::{ContextSize, NewlineSeparator};
use gix::diff::blob::UnifiedDiff;
use gix::objs::tree::EntryMode;
use gix::odb::store::RefreshMode;
use gix::prelude::ObjectIdExt;

pub fn tree(
    mut repo: gix::Repository,
    out: &mut dyn std::io::Write,
    old_treeish: BString,
    new_treeish: BString,
) -> anyhow::Result<()> {
    repo.object_cache_size_if_unset(repo.compute_object_cache_size_for_tree_diffs(&**repo.index_or_empty()?));
    repo.objects.refresh = RefreshMode::Never;

    let old_tree_id = repo.rev_parse_single(old_treeish.as_bstr())?;
    let new_tree_id = repo.rev_parse_single(new_treeish.as_bstr())?;

    let old_tree = old_tree_id.object()?.peel_to_tree()?;
    let new_tree = new_tree_id.object()?.peel_to_tree()?;

    let changes = repo.diff_tree_to_tree(&old_tree, &new_tree, None)?;

    writeln!(
        out,
        "Diffing trees `{old_treeish}` ({old_tree_id}) -> `{new_treeish}` ({new_tree_id})\n"
    )?;
    write_changes(&repo, out, changes)?;

    Ok(())
}

fn write_changes(
    repo: &gix::Repository,
    mut out: impl std::io::Write,
    changes: Vec<gix::diff::tree_with_rewrites::Change>,
) -> Result<(), std::io::Error> {
    for change in changes {
        match change {
            gix::diff::tree_with_rewrites::Change::Addition {
                location,
                id,
                entry_mode,
                ..
            } => {
                writeln!(out, "A: {}", typed_location(location, entry_mode))?;
                writeln!(out, "  {}", id.attach(repo).shorten_or_id())?;
                writeln!(out, "  -> {:o}", entry_mode.0)?;
            }
            gix::diff::tree_with_rewrites::Change::Deletion {
                location,
                id,
                entry_mode,
                ..
            } => {
                writeln!(out, "D: {}", typed_location(location, entry_mode))?;
                writeln!(out, "  {}", id.attach(repo).shorten_or_id())?;
                writeln!(out, "  {:o} ->", entry_mode.0)?;
            }
            gix::diff::tree_with_rewrites::Change::Modification {
                location,
                previous_id,
                id,
                previous_entry_mode,
                entry_mode,
            } => {
                writeln!(out, "M: {}", typed_location(location, entry_mode))?;
                writeln!(
                    out,
                    "  {previous_id} -> {id}",
                    previous_id = previous_id.attach(repo).shorten_or_id(),
                    id = id.attach(repo).shorten_or_id()
                )?;
                if previous_entry_mode != entry_mode {
                    writeln!(out, "  {:o} -> {:o}", previous_entry_mode.0, entry_mode.0)?;
                }
            }
            gix::diff::tree_with_rewrites::Change::Rewrite {
                source_location,
                source_id,
                id,
                location,
                source_entry_mode,
                entry_mode,
                ..
            } => {
                writeln!(
                    out,
                    "R: {source} -> {dest}",
                    source = typed_location(source_location, source_entry_mode),
                    dest = typed_location(location, entry_mode)
                )?;
                writeln!(
                    out,
                    "  {source_id} -> {id}",
                    source_id = source_id.attach(repo).shorten_or_id(),
                    id = id.attach(repo).shorten_or_id()
                )?;
                if source_entry_mode != entry_mode {
                    writeln!(out, "  {:o} -> {:o}", source_entry_mode.0, entry_mode.0)?;
                }
            }
        }
    }

    Ok(())
}

fn typed_location(mut location: BString, mode: EntryMode) -> BString {
    if mode.is_tree() {
        location.push(b'/');
    }
    location
}

pub fn file(
    mut repo: gix::Repository,
    out: &mut dyn std::io::Write,
    old_revspec: BString,
    new_revspec: BString,
) -> Result<(), anyhow::Error> {
    repo.object_cache_size_if_unset(repo.compute_object_cache_size_for_tree_diffs(&**repo.index_or_empty()?));
    repo.objects.refresh = RefreshMode::Never;

    let old_resolved_revspec = repo.rev_parse(old_revspec.as_bstr())?;
    let new_resolved_revspec = repo.rev_parse(new_revspec.as_bstr())?;

    let old_blob_id = old_resolved_revspec
        .single()
        .context(format!("rev-spec '{old_revspec}' must resolve to a single object"))?;
    let new_blob_id = new_resolved_revspec
        .single()
        .context(format!("rev-spec '{new_revspec}' must resolve to a single object"))?;

    let (old_path, _) = old_resolved_revspec
        .path_and_mode()
        .context(format!("rev-spec '{old_revspec}' must contain a path"))?;
    let (new_path, _) = new_resolved_revspec
        .path_and_mode()
        .context(format!("rev-spec '{new_revspec}' must contain a path"))?;

    let mut resource_cache = repo.diff_resource_cache(
        gix::diff::blob::pipeline::Mode::ToGitUnlessBinaryToTextIsPresent,
        Default::default(),
    )?;

    resource_cache.set_resource(
        old_blob_id.into(),
        gix::object::tree::EntryKind::Blob,
        old_path,
        gix::diff::blob::ResourceKind::OldOrSource,
        &repo.objects,
    )?;
    resource_cache.set_resource(
        new_blob_id.into(),
        gix::object::tree::EntryKind::Blob,
        new_path,
        gix::diff::blob::ResourceKind::NewOrDestination,
        &repo.objects,
    )?;

    let outcome = resource_cache.prepare_diff()?;

    use gix::diff::blob::platform::prepare_diff::Operation;

    let algorithm = match outcome.operation {
        Operation::InternalDiff { algorithm } => algorithm,
        Operation::ExternalCommand { .. } => {
            unreachable!("We disabled that")
        }
        Operation::SourceOrDestinationIsBinary => {
            anyhow::bail!("Source or destination is binary and we can't diff that")
        }
    };

    let interner = gix::diff::blob::intern::InternedInput::new(
        tokens_for_diffing(outcome.old.data.as_slice().unwrap_or_default()),
        tokens_for_diffing(outcome.new.data.as_slice().unwrap_or_default()),
    );

    let unified_diff = UnifiedDiff::new(
        &interner,
        String::new(),
        NewlineSeparator::AfterHeaderAndLine("\n"),
        ContextSize::symmetrical(3),
    );

    let unified_diff = gix::diff::blob::diff(algorithm, &interner, unified_diff)?;

    out.write_all(unified_diff.as_bytes())?;

    Ok(())
}

pub(crate) fn tokens_for_diffing(data: &[u8]) -> impl TokenSource<Token = &[u8]> {
    gix::diff::blob::sources::byte_lines(data)
}
