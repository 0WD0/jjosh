//! A benchmark for generating a pack from many loose objects, for issue #2611.
//!
//! It isolates the two phases the issue is concerned with:
//!   1. counting: walking the given object ids into the `Vec<output::Count>` (the O(N) setup).
//!   2. writing:  turning those counts into entries and streaming them out as a pack.
//!
//! Besides timing each phase with criterion, it prints a one-off report of the peak heap
//! allocation observed during each phase and of the resulting pack size, so the memory footprint
//! and the (missing) delta-compression discussed in the issue are grounded in concrete numbers.
//!
//! Run with `cargo bench -p gix-pack --bench pack_generation`.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    io,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group};
use gix_features::{parallel::InOrderIter, progress};
use gix_object::Write as _;
use gix_pack::data::output::{self, bytes::FromEntriesIter, count, entry};

/// Object counts to exercise. Kept modest so the benchmark stays runnable while still showing how
/// the phases scale; raise locally to probe larger, more degenerate repositories.
const OBJECT_COUNTS: &[usize] = &[1_000, 10_000, 50_000];

// A global allocator that records the peak number of live bytes, so the memory footprint of each
// phase can be reported. Tracking is always on, but uses relaxed atomics and so adds only a small,
// constant overhead to the timings, which does not affect how they scale.
struct PeakTracking;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for PeakTracking {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            let live = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(live, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }
}

#[global_allocator]
static ALLOCATOR: PeakTracking = PeakTracking;

/// Set the recorded peak to the current number of live bytes, so a later [`peak_bytes`] reports the
/// maximum reached after this call.
fn reset_peak() {
    PEAK.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
}

fn peak_bytes() -> usize {
    PEAK.load(Ordering::Relaxed)
}

/// Create a fresh object database populated with `count` unique loose blobs, returning the
/// temporary directory (which must be kept alive by the caller), the handle, and the written ids.
fn loose_odb(count: usize) -> (tempfile::TempDir, gix_odb::Handle, Vec<gix_hash::ObjectId>) {
    let dir = tempfile::tempdir().expect("can create a temporary directory");
    let objects_dir = dir.path().join("objects");
    std::fs::create_dir_all(&objects_dir).expect("can create the objects directory");
    let odb = gix_odb::at(objects_dir).expect("can open the object database");
    let ids = (0..count)
        .map(|i| {
            let content = format!("loose object {i} with a little bit of unique payload\n");
            odb.write_buf(gix_object::Kind::Blob, content.as_bytes())
                .expect("can write a loose object")
        })
        .collect();
    (dir, odb, ids)
}

/// Phase 1: collect the counts for `ids` without object expansion - exactly the loose objects given.
fn count_objects(odb: &gix_odb::Handle, ids: &[gix_hash::ObjectId]) -> Vec<output::Count> {
    let mut input = ids.iter().copied().map(Ok);
    let (counts, _outcome) = count::objects_unthreaded(
        odb,
        &mut input,
        &progress::Discard,
        &AtomicBool::new(false),
        count::objects::ObjectExpansion::AsIs,
    )
    .expect("counting loose objects succeeds");
    counts
}

/// A writer that only remembers how many bytes were written, to measure the pack size without
/// touching the disk.
#[derive(Default)]
struct CountingSink(u64);

impl io::Write for CountingSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0 += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Phase 2: turn `counts` into entries and stream them out as a pack, returning the pack size.
fn write_pack(odb: &gix_odb::Handle, counts: Vec<output::Count>) -> u64 {
    let num_objects = counts.len() as u32;
    let entries = InOrderIter::from(entry::iter_from_counts(
        counts,
        odb.clone(),
        Box::new(progress::Discard),
        entry::iter_from_counts::Options {
            mode: entry::iter_from_counts::Mode::PackCopyAndBaseObjects,
            ..Default::default()
        },
    ));
    let mut sink = CountingSink::default();
    let mut iter = FromEntriesIter::new(
        entries,
        &mut sink,
        num_objects,
        gix_pack::data::Version::default(),
        gix_hash::Kind::Sha1,
    );
    for chunk in iter.by_ref() {
        chunk.expect("writing a pack chunk succeeds");
    }
    sink.0
}

fn bench_count(c: &mut Criterion) {
    let mut group = c.benchmark_group("count-loose-objects");
    for &n in OBJECT_COUNTS {
        let (_dir, odb, ids) = loose_odb(n);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| count_objects(&odb, &ids));
        });
    }
    group.finish();
}

fn bench_write(c: &mut Criterion) {
    let mut group = c.benchmark_group("write-pack");
    for &n in OBJECT_COUNTS {
        let (_dir, odb, ids) = loose_odb(n);
        let counts = count_objects(&odb, &ids);
        let pack_size = write_pack(&odb, counts.clone());
        group.throughput(Throughput::Bytes(pack_size));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter_batched(
                || counts.clone(),
                |counts| write_pack(&odb, counts),
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

/// Print a one-off table of the peak heap allocation per phase and the resulting pack size, to
/// ground the memory and pack-size discussion in #2611 in concrete numbers.
fn report_memory_and_size() {
    eprintln!("\n# pack generation from loose objects (issue #2611)");
    eprintln!(
        "{:>10} {:>16} {:>16} {:>12}",
        "objects", "count peak KiB", "write peak KiB", "pack KiB"
    );
    for &n in OBJECT_COUNTS {
        let (_dir, odb, ids) = loose_odb(n);

        reset_peak();
        let counts = count_objects(&odb, &ids);
        let count_peak = peak_bytes();

        reset_peak();
        let pack_size = write_pack(&odb, counts);
        let write_peak = peak_bytes();

        eprintln!(
            "{:>10} {:>16} {:>16} {:>12}",
            n,
            count_peak / 1024,
            write_peak / 1024,
            pack_size / 1024
        );
    }
    eprintln!();
}

criterion_group!(benches, bench_count, bench_write);

fn main() {
    report_memory_and_size();
    benches();
    Criterion::default().configure_from_args().final_summary();
}
