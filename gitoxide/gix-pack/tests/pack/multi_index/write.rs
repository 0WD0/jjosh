use std::{path::PathBuf, sync::atomic::AtomicBool};

use gix_features::progress;
use gix_testtools::fixture_path;

use crate::hex_to_id;

/// Writes a multi-index from the static SHA-1 pack indices, with pinned SHA-1 expectations.
/// The SHA-256 counterpart lives in `from_a_hash_parameterized_pack` below.
#[test]
fn from_paths() -> crate::Result {
    let dir = gix_testtools::tempfile::TempDir::new()?;
    let input_indices = std::fs::read_dir(fixture_path("objects/pack"))?
        .filter_map(|r| {
            r.ok()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(std::ffi::OsStr::to_str).unwrap_or("") == "idx")
        })
        .collect::<Vec<_>>();
    assert_eq!(input_indices.len(), 3);
    let output_path = dir.path().join("multi-pack-index");
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output_path)?;
    let outcome = gix_pack::multi_index::write_from_index_paths(
        input_indices.clone(),
        &mut out,
        &mut progress::Discard,
        &AtomicBool::new(false),
        gix_pack::multi_index::write::Options {
            object_hash: gix_hash::Kind::Sha1,
        },
    )?;

    assert_eq!(
        outcome.multi_index_checksum,
        hex_to_id("d34d327039a3554f8a644b29e07b903fa71ef269")
    );

    let file = gix_pack::multi_index::File::at(output_path, None)?;
    assert_eq!(file.num_indices(), 3);
    assert_eq!(
        file.index_names(),
        vec![
            PathBuf::from("pack-11fdfa9e156ab73caae3b6da867192221f2089c2.idx"),
            PathBuf::from("pack-a2bf8e71d8c18879e499335762dd95119d93d9f1.idx"),
            PathBuf::from("pack-c0438c19fb16422b6bbcce24387b3264416d485b.idx"),
        ]
    );
    assert_eq!(file.num_objects(), 139);
    assert_eq!(file.checksum(), outcome.multi_index_checksum);

    for index in &input_indices {
        std::fs::copy(index, dir.path().join(index.file_name().expect("present")))?;
        let pack = index.with_extension("pack");
        std::fs::copy(&pack, dir.path().join(pack.file_name().expect("present")))?;
    }

    assert_eq!(
        file.verify_integrity(&mut progress::Discard, &AtomicBool::new(false), Default::default())?
            .actual_index_checksum,
        outcome.multi_index_checksum
    );

    let outcome = file.verify_integrity_fast(&mut progress::Discard, &AtomicBool::new(false))?;

    assert_eq!(outcome, file.checksum());
    Ok(())
}

/// Like `from_paths`, but sources its input index from the hash-parameterized fixture so the
/// writer runs under both SHA-1 and SHA-256. The fixture's gc leaves one pack, hence one index.
#[test]
fn from_a_hash_parameterized_pack() -> crate::Result {
    let object_hash = crate::object_hash();
    let pack_dir = crate::scripted_fixture_read_only("make_pack_gen_repo_multi_index.sh")?.join(".git/objects/pack");
    let input_indices = std::fs::read_dir(&pack_dir)?
        .filter_map(|r| {
            r.ok()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(std::ffi::OsStr::to_str) == Some("idx"))
        })
        .collect::<Vec<_>>();
    assert_eq!(input_indices.len(), 1, "the aggressive gc leaves a single pack");

    let dir = gix_testtools::tempfile::TempDir::new()?;
    let output_path = dir.path().join("multi-pack-index");
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output_path)?;
    let outcome = gix_pack::multi_index::write_from_index_paths(
        input_indices.clone(),
        &mut out,
        &mut progress::Discard,
        &AtomicBool::new(false),
        gix_pack::multi_index::write::Options { object_hash },
    )?;

    let file = gix_pack::multi_index::File::at(output_path, None)?;
    assert_eq!(
        file.object_hash(),
        object_hash,
        "the writer records the requested object hash"
    );
    assert_eq!(file.num_indices(), 1);
    assert_eq!(file.num_objects(), 868);
    assert_eq!(file.checksum(), outcome.multi_index_checksum);

    // Place the referenced pack and index next to the multi-index so integrity can resolve them.
    for index in &input_indices {
        std::fs::copy(index, dir.path().join(index.file_name().expect("present")))?;
        let pack = index.with_extension("pack");
        std::fs::copy(&pack, dir.path().join(pack.file_name().expect("present")))?;
    }

    assert_eq!(
        file.verify_integrity(&mut progress::Discard, &AtomicBool::new(false), Default::default())?
            .actual_index_checksum,
        outcome.multi_index_checksum
    );
    assert_eq!(
        file.verify_integrity_fast(&mut progress::Discard, &AtomicBool::new(false))?,
        file.checksum()
    );
    Ok(())
}
