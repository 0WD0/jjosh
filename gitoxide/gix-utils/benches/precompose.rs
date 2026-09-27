// Compare revisions with `cargo bench -p gix-utils --bench precompose -- --save-baseline before`
// and `cargo bench -p gix-utils --bench precompose -- --baseline before`.
// Measures normalization and disposal of the result, without filesystem I/O.
use std::{hint::black_box, path::Path};

use criterion::{Criterion, criterion_group, criterion_main};

fn precompose(c: &mut Criterion) {
    let mut group = c.benchmark_group("precompose/filename");
    for (name, input) in [
        ("ascii", "git_status.rs"),
        ("nfc", "Überwachung im digitalen Zeitalter.md"),
        ("nfd", "U\u{308}berwachung im digitalen Zeitalter.md"),
        ("several_nfd", "A\u{308}O\u{308}U\u{308}e\u{301}.md"),
        ("emoji_first", "🎥 U\u{308}berwachung im digitalen Zeitalter.md"),
        ("emoji_last", "U\u{308}berwachung im digitalen Zeitalter 🎥.md"),
        ("nfc_with_mark", "ä\u{315}.md"),
        ("nfc_quick_check_maybe", "äq\u{308}.md"),
        ("noncanonical_marks", "ا\u{651}\u{64f}.md"),
        ("hangul_nfc", "한글.md"),
        ("hangul_nfd", "\u{1112}\u{1161}\u{11ab}\u{1100}\u{1173}\u{11af}.md"),
    ] {
        group.bench_function(name, |b| {
            b.iter(|| gix_utils::str::precompose(black_box(input).into()));
        });
    }
    group.finish();

    let mut group = c.benchmark_group("precompose/path");
    for (name, input) in [
        ("ascii", "src/modules/git_status.rs"),
        (
            "ascii_long",
            "/Users/developer/src/github.com/GitoxideLabs/gitoxide/gix-utils/src/str.rs",
        ),
        ("nfc", "Teaching/Überwachung im digitalen Zeitalter.md"),
        ("nfd", "Teaching/U\u{308}berwachung im digitalen Zeitalter.md"),
        ("several_nfd", "A\u{308}/O\u{308}/U\u{308}/e\u{301}.md"),
        ("reported", "Teaching/🎥 U\u{308}berwachung im digitalen Zeitalter.md"),
        ("mixed", "U\u{308}bungen/🎥 U\u{308}berwachung/U\u{308}bung.md"),
    ] {
        group.bench_function(name, |b| {
            b.iter(|| gix_utils::str::precompose_path(black_box(Path::new(input)).into()));
        });
    }
    group.finish();
}

criterion_group!(benches, precompose);
criterion_main!(benches);
