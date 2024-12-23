use std::{ffi::OsStr, path::PathBuf};

use anyhow::anyhow;
use gix::bstr::BStr;

pub fn blame_file(mut repo: gix::Repository, file: &OsStr, out: impl std::io::Write) -> anyhow::Result<()> {
    repo.object_cache_size_if_unset(repo.compute_object_cache_size_for_tree_diffs(&**repo.index_or_empty()?));

    let suspect = repo.head()?.peel_to_commit_in_place()?;
    let traverse =
        gix::traverse::commit::topo::Builder::from_iters(&repo.objects, [suspect.id], None::<Vec<gix::ObjectId>>)
            .build()?;
    let mut resource_cache = repo.diff_resource_cache_for_tree_diff()?;

    let work_dir: PathBuf = repo
        .work_dir()
        .ok_or_else(|| anyhow!("blame needs a workdir, but there is none"))?
        .into();
    let file_path: &BStr = gix::path::os_str_into_bstr(file)?;

    let outcome = gix::blame::file(
        &repo.objects,
        traverse,
        &mut resource_cache,
        work_dir.clone(),
        file_path,
    )?;
    write_blame_entries(out, outcome)?;

    Ok(())
}

fn write_blame_entries(mut out: impl std::io::Write, outcome: gix::blame::Outcome) -> Result<(), std::io::Error> {
    for (entry, lines_in_hunk) in outcome.entries_with_lines() {
        for ((actual_lno, source_lno), line) in entry
            .range_in_blamed_file
            .zip(entry.range_in_original_file)
            .zip(lines_in_hunk)
        {
            write!(
                out,
                "{short_id} {line_no} {src_line_no} {line}",
                line_no = actual_lno + 1,
                src_line_no = source_lno + 1,
                short_id = entry.commit_id.to_hex_with_len(8),
            )?;
        }
    }

    Ok(())
}
