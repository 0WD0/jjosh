//! A benchmark for generating a pack from many loose objects, for issue #2611.
//!
//! It separates the phases of a `gix pack create`-style generation so their cost can be looked at
//! independently:
//!   - `discover`: walking the object database to enumerate the loose object ids (`odb.iter()`).
//!   - `count`:    turning those ids into a `Vec<output::Count>`, which loads each object's header.
//!   - `write`:    resolving locations, sorting and encoding the counts into a pack byte stream.
//!
//! A note on where the work lands: the counts are *sorted* and their pack locations resolved inside
//! `iter_from_counts` - that is, in the `write` phase, not in `count`. So the "sorting" cost the
//! issue mentions is attributed to `write` here, not `count`.
//!
//! Alongside the criterion timings, `main` prints a one-off report of the peak *heap* allocation
//! seen during each phase and the resulting pack size. The peak-heap figure is a lower bound on
//! RSS: it counts live bytes from the global allocator only, excluding thread stacks, memory-mapped
//! pack pages and any memory the allocator retains after freeing.
//!
//! Caveats for reading the numbers:
//!   - a global tracking allocator is installed, adding a small constant overhead to every
//!     allocation; the criterion timings are thus slightly inflated, though consistently, so the
//!     scaling across object counts is unaffected;
//!   - the `write` phase is multi-threaded, so its wall-clock time varies with the number of cores;
//!   - the fixture is flat, near-identical blobs written to a sink that discards output, so the
//!     `write` peak is a floor and does not exercise writer back-pressure; and
//!   - `AsIs` input with no pre-existing pack never attempts delta compression, so the pack size is
//!     the un-deltified baseline - quantifying the delta gap would need a `git`-produced pack of the
//!     same objects to compare against.
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

// A global allocator that records the peak number of live bytes, so the heap footprint of each
// phase can be reported. Tracking is always on; it uses relaxed atomics and so adds only a small,
// constant overhead, which does not change how the timings scale.
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
/// temporary directory (which must be kept alive by the caller) and the handle.
fn loose_odb(count: usize) -> (tempfile::TempDir, gix_odb::Handle) {
    let dir = tempfile::tempdir().expect("can create a temporary directory");
    let objects_dir = dir.path().join("objects");
    std::fs::create_dir_all(&objects_dir).expect("can create the objects directory");
    let odb = gix_odb::at(objects_dir).expect("can open the object database");
    for i in 0..count {
        let content = format!("loose object {i} with a little bit of unique payload\n");
        odb.write_buf(gix_object::Kind::Blob, content.as_bytes())
            .expect("can write a loose object");
    }
    (dir, odb)
}

/// Phase 1: walk the database to enumerate all (here, loose) object ids.
fn discover(odb: &gix_odb::Handle) -> Vec<gix_hash::ObjectId> {
    odb.iter()
        .expect("can iterate the object database")
        .filter_map(Result::ok)
        .collect()
}

/// Phase 2: collect the counts for `ids` without object expansion - exactly the objects given.
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
/// touching the disk. It imposes no back-pressure, so it does not exercise the writer itself.
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

/// Phase 3: resolve, sort and encode `counts` into a pack stream, returning the pack size in bytes.
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

fn bench_discover(c: &mut Criterion) {
    let mut group = c.benchmark_group("discover-loose-objects");
    for &n in OBJECT_COUNTS {
        let (_dir, odb) = loose_odb(n);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| discover(&odb));
        });
    }
    group.finish();
}

fn bench_count(c: &mut Criterion) {
    let mut group = c.benchmark_group("count-loose-objects");
    for &n in OBJECT_COUNTS {
        let (_dir, odb) = loose_odb(n);
        let ids = discover(&odb);
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
        let (_dir, odb) = loose_odb(n);
        let ids = discover(&odb);
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
    eprintln!("# peak columns are peak heap allocation during the phase (KiB), a lower bound on RSS");
    eprintln!(
        "{:>10} {:>14} {:>14} {:>14} {:>12}",
        "objects", "discover peak", "count peak", "write peak", "pack KiB"
    );
    for &n in OBJECT_COUNTS {
        let (_dir, odb) = loose_odb(n);

        reset_peak();
        let ids = discover(&odb);
        let discover_peak = peak_bytes();

        reset_peak();
        let counts = count_objects(&odb, &ids);
        let count_peak = peak_bytes();

        reset_peak();
        let pack_size = write_pack(&odb, counts);
        let write_peak = peak_bytes();

        eprintln!(
            "{:>10} {:>14} {:>14} {:>14} {:>12}",
            n,
            discover_peak / 1024,
            count_peak / 1024,
            write_peak / 1024,
            pack_size / 1024
        );
    }
    eprintln!();
}

criterion_group!(benches, bench_discover, bench_count, bench_write);

fn main() {
    report_memory_and_size();
    benches();
    Criterion::default().configure_from_args().final_summary();
}
