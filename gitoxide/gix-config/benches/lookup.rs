use std::hint::black_box;

use bstr::{BString, ByteSlice};
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use gix_config::File;

const SIZES: [usize; 3] = [10, 100, 1_000];
const VALUE_COUNTS: [usize; 2] = [5, 25];

fn file_with_distinct_sections(num_sections: usize) -> (File, Vec<String>) {
    let mut file = File::default();
    let mut section_names = Vec::with_capacity(num_sections);
    for index in 0..num_sections {
        let section_name = format!("section-{index}");
        file.new_section(&section_name, None)
            .expect("generated section names are valid");
        section_names.push(section_name);
    }
    (file, section_names)
}

fn file_with_distinct_subsections(num_subsections: usize) -> (File, Vec<BString>) {
    let mut file = File::default();
    let mut subsection_names = Vec::with_capacity(num_subsections);
    for index in 0..num_subsections {
        let subsection_name = index.to_string();
        file.new_section("remote", subsection_name.as_str())
            .expect("generated subsection names are valid");
        subsection_names.push(subsection_name.into());
    }
    (file, subsection_names)
}

fn file_with_matching_and_unrelated_sections(num_matching_sections: usize) -> File {
    let mut file = File::default();
    for index in 0..num_matching_sections {
        file.new_section("remote", index.to_string())
            .expect("generated subsection names are valid");
        file.new_section(format!("unrelated-{index}"), None)
            .expect("generated section names are valid");
    }
    file
}

fn minimal_file_with_values(num_values: usize) -> File {
    let mut file = File::default();
    file.new_section("user", None)
        .expect("static section name is valid")
        .push(
            "name".try_into().expect("static value name is valid"),
            Some("A U Thor".into()),
        )
        .expect("benchmark data fits into the backing buffer");

    {
        let mut section = file.new_section("core", None).expect("static section name is valid");
        for index in 0..num_values {
            section
                .push(
                    format!("key-{index}")
                        .try_into()
                        .expect("generated value name is valid"),
                    Some("v".into()),
                )
                .expect("benchmark data fits into the backing buffer");
        }
    }

    {
        let mut section = file
            .new_section("remote", "x")
            .expect("static section and subsection names are valid");
        for index in 0..num_values {
            section
                .push(
                    format!("key-{index}")
                        .try_into()
                        .expect("generated value name is valid"),
                    Some("v".into()),
                )
                .expect("benchmark data fits into the backing buffer");
        }
    }
    file
}

fn minimal_file_with_multivar(num_values: usize) -> File {
    let mut file = File::default();
    file.new_section("user", None)
        .expect("static section name is valid")
        .push("name".try_into().expect("static value name is valid"), Some("v".into()))
        .expect("benchmark data fits into the backing buffer");
    file.new_section("core", None)
        .expect("static section name is valid")
        .push(
            "editor".try_into().expect("static value name is valid"),
            Some("v".into()),
        )
        .expect("benchmark data fits into the backing buffer");

    let mut section = file
        .new_section("remote", "x")
        .expect("static section and subsection names are valid");
    for _ in 0..num_values {
        section
            .push(
                "fetch".try_into().expect("static value name is valid"),
                Some("v".into()),
            )
            .expect("benchmark data fits into the backing buffer");
    }
    file
}

fn lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("config lookup");

    for size in SIZES {
        group.throughput(Throughput::Elements(size as u64));

        let (file, section_names) = file_with_distinct_sections(size);
        group.bench_with_input(BenchmarkId::new("section by name", size), &size, |b, _| {
            b.iter(|| {
                for section_name in &section_names {
                    black_box(
                        file.section(black_box(section_name.as_str()), None)
                            .expect("target section exists"),
                    );
                }
            });
        });

        let (file, subsection_names) = file_with_distinct_subsections(size);
        group.bench_with_input(BenchmarkId::new("section by subsection", size), &size, |b, _| {
            b.iter(|| {
                for subsection_name in &subsection_names {
                    black_box(
                        file.section("remote", black_box(subsection_name.as_bstr()))
                            .expect("target subsection exists"),
                    );
                }
            });
        });

        let file = file_with_matching_and_unrelated_sections(size);
        group.bench_with_input(BenchmarkId::new("all sections by name", size), &size, |b, _| {
            b.iter(|| {
                let count = file
                    .sections_by_name(black_box("remote"))
                    .expect("target sections exist")
                    .fold(0, |count, section| {
                        black_box(section);
                        count + 1
                    });
                black_box(count)
            });
        });
    }

    group.finish();
}

fn value_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("complete value lookup");

    for num_values in VALUE_COUNTS {
        let file = minimal_file_with_values(num_values);
        let last_key = format!("key-{}", num_values - 1);
        // These are complete lookups through the section index and section body. The first value makes the
        // reverse scan traverse the entire body. `raw_value_by()` returns an owned `BString`, so allocation
        // can't be avoided here; one-byte values make that allocation and the subsequent copy as cheap as possible.
        group.bench_with_input(
            BenchmarkId::new("first value without subsection", num_values),
            &num_values,
            |b, _| {
                b.iter(|| {
                    black_box(
                        file.raw_value_by(black_box("core"), None, black_box("key-0"))
                            .expect("target value exists"),
                    )
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("last value without subsection", num_values),
            &num_values,
            |b, _| {
                b.iter(|| {
                    black_box(
                        file.raw_value_by(black_box("core"), None, black_box(last_key.as_str()))
                            .expect("target value exists"),
                    )
                });
            },
        );

        let subsection = "x".as_bytes().as_bstr();
        group.bench_with_input(
            BenchmarkId::new("first value with subsection", num_values),
            &num_values,
            |b, _| {
                b.iter(|| {
                    black_box(
                        file.raw_value_by(black_box("remote"), black_box(subsection), black_box("key-0"))
                            .expect("target value exists"),
                    )
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("last value with subsection", num_values),
            &num_values,
            |b, _| {
                b.iter(|| {
                    black_box(
                        file.raw_value_by(black_box("remote"), black_box(subsection), black_box(last_key.as_str()))
                            .expect("target value exists"),
                    )
                });
            },
        );
    }

    group.finish();

    let mut group = c.benchmark_group("complete multi-value lookup");
    let subsection = "x".as_bytes().as_bstr();
    for num_values in VALUE_COUNTS {
        group.throughput(Throughput::Elements(num_values as u64));
        let file = minimal_file_with_multivar(num_values);
        group.bench_with_input(
            BenchmarkId::new("remote fetch values", num_values),
            &num_values,
            |b, _| {
                b.iter(|| {
                    black_box(
                        file.raw_values_by(black_box("remote"), black_box(subsection), black_box("fetch"))
                            .expect("target values exist"),
                    )
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, lookup, value_lookup);
criterion_main!(benches);
