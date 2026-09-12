use std::{fmt::Write, hint::black_box, path::Path};

use bstr::ByteSlice;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use gix_attributes::{
    Search,
    search::{MetadataCollection, Outcome},
};
use gix_glob::pattern::Case;

fn attributes(num_entries: usize, file_index: usize) -> Vec<u8> {
    let mut out = String::with_capacity(num_entries * 32);
    for entry_index in 0..num_entries {
        writeln!(out, "*.rs attr-{file_index}-{entry_index}").expect("writing to a string cannot fail");
    }
    out.into_bytes()
}

fn lookup(search: &Search, collection: &MetadataCollection, path: &str) -> usize {
    let mut outcome = Outcome::default();
    outcome.initialize(collection);
    assert!(search.pattern_matching_relative_path(
        path.as_bytes().as_bstr(),
        Case::Sensitive,
        Some(false),
        &mut outcome
    ));
    outcome.iter().count()
}

fn single_file(c: &mut Criterion) {
    let mut group = c.benchmark_group("attribute lookup/single file");
    for num_entries in [1, 10, 100, 1_000] {
        let mut search = Search::default();
        let mut collection = MetadataCollection::default();
        search.add_patterns_buffer(
            &attributes(num_entries, 0),
            ".gitattributes".into(),
            None,
            &mut collection,
            true,
        );

        assert_eq!(lookup(&search, &collection, "file.rs"), num_entries);
        group.throughput(Throughput::Elements(num_entries as u64));
        group.bench_with_input(BenchmarkId::from_parameter(num_entries), &num_entries, |b, _| {
            let mut outcome = Outcome::default();
            outcome.initialize(&collection);
            b.iter(|| {
                outcome.reset();
                assert!(search.pattern_matching_relative_path(
                    black_box(b"file.rs".as_bstr()),
                    Case::Sensitive,
                    Some(false),
                    &mut outcome,
                ));
                assert_eq!(black_box(outcome.iter().count()), num_entries);
            });
        });
    }
    group.finish();
}

fn five_file_hierarchy(c: &mut Criterion) {
    const ENTRIES: [usize; 5] = [1_024, 512, 256, 128, 64];
    const DIRECTORIES: [&str; 5] = ["", "a", "a/b", "a/b/c", "a/b/c/d"];

    let mut search = Search::default();
    let mut collection = MetadataCollection::default();
    for (file_index, (num_entries, directory)) in ENTRIES.into_iter().zip(DIRECTORIES).enumerate() {
        let source = if directory.is_empty() {
            ".gitattributes".into()
        } else {
            Path::new(directory).join(".gitattributes")
        };
        search.add_patterns_buffer(
            &attributes(num_entries, file_index),
            source,
            Some(Path::new("")),
            &mut collection,
            true,
        );
    }

    let num_entries = ENTRIES.into_iter().sum::<usize>();
    let path = "a/b/c/d/file.rs";
    assert_eq!(lookup(&search, &collection, path), num_entries);

    let mut group = c.benchmark_group("attribute lookup/five file hierarchy");
    group.throughput(Throughput::Elements(num_entries as u64));
    group.bench_function("1024-512-256-128-64", |b| {
        let mut outcome = Outcome::default();
        outcome.initialize(&collection);
        b.iter(|| {
            outcome.reset();
            assert!(search.pattern_matching_relative_path(
                black_box(path.as_bytes().as_bstr()),
                Case::Sensitive,
                Some(false),
                &mut outcome,
            ));
            assert_eq!(black_box(outcome.iter().count()), num_entries);
        });
    });
    group.finish();
}

criterion_group!(benches, single_file, five_file_hierarchy);
criterion_main!(benches);
