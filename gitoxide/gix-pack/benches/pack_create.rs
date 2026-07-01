//! A benchmark for generating a pack from many in-memory objects, for issue #2611.
//!
//! It separates the phases of a `gix pack create`-style generation so their cost can be looked at
//! independently:
//!   - `count`:    turning those ids into a `Vec<output::Count>`, which loads each object's header.
//!   - `write`:    resolving locations, sorting and encoding the counts into a pack byte stream.
//!
//! Note that the discovery phase is as fast as the object database can traverse objects, so nothing to test here
//! and we use in-memory speeds for this.
use std::{
    io,
    sync::{Arc, atomic::AtomicBool},
};

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group};
use gix_features::{parallel::InOrderIter, progress};
use gix_object::Write as _;
use gix_pack::data::output::{self, bytes::FromEntriesIter, count, entry};

/// Object counts to exercise. Kept modest so the benchmark stays runnable while still showing how
/// the phases scale; raise locally to probe larger, more degenerate repositories.
const OBJECT_COUNTS: &[usize] = &[1_000, 10_000, 50_000];

/// Adapt in-memory objects to `gix_pack::Find`. `gix_odb::memory::Proxy` is still used to populate
/// the storage, but `count` and `write` need pack-location APIs as well as cheap clones for write
/// workers.
#[derive(Clone)]
struct MemoryOdb {
    objects: Arc<gix_odb::memory::Storage>,
}

impl gix_pack::Find for MemoryOdb {
    fn contains(&self, id: &gix_hash::oid) -> bool {
        self.objects.contains_key(id)
    }

    fn try_find_cached<'a>(
        &self,
        id: &gix_hash::oid,
        buffer: &'a mut Vec<u8>,
        _pack_cache: &mut dyn gix_pack::cache::DecodeEntry,
    ) -> Result<Option<(gix_object::Data<'a>, Option<gix_pack::data::entry::Location>)>, gix_object::find::Error> {
        Ok(self.objects.get(id).map(|(kind, data)| {
            buffer.clear();
            buffer.extend_from_slice(data);
            (
                gix_object::Data {
                    kind: *kind,
                    object_hash: id.kind(),
                    data: buffer,
                },
                None,
            )
        }))
    }

    fn location_by_oid(&self, _id: &gix_hash::oid, _buf: &mut Vec<u8>) -> Option<gix_pack::data::entry::Location> {
        None
    }

    fn pack_offsets_and_oid(&self, _pack_id: u32) -> Option<Vec<(gix_pack::data::Offset, gix_hash::ObjectId)>> {
        None
    }

    fn entry_by_location(&self, _location: &gix_pack::data::entry::Location) -> Option<gix_pack::find::Entry> {
        None
    }
}

/// Create a fresh in-memory object database populated with `count` unique blobs.
fn memory_odb(count: usize) -> (MemoryOdb, Vec<gix_hash::ObjectId>) {
    let mut odb = gix_odb::memory::Proxy::new(gix_odb::sink(gix_hash::Kind::Sha1), gix_hash::Kind::Sha1);
    let mut ids = Vec::with_capacity(count);
    for i in 0..count {
        let content = format!("memory object {i} with a little bit of unique payload\n");
        let id = odb
            .write_buf(gix_object::Kind::Blob, content.as_bytes())
            .expect("can write an in-memory object");
        ids.push(id);
    }
    let objects = odb
        .take_object_memory()
        .expect("in-memory object storage is still enabled");
    (
        MemoryOdb {
            objects: Arc::new(objects),
        },
        ids,
    )
}

/// Phase 1: collect the counts for `ids` without object expansion - exactly the objects given.
fn count_objects_unthreaded(odb: &MemoryOdb, ids: &[gix_hash::ObjectId]) -> Vec<output::Count> {
    let mut input = ids.iter().copied().map(Ok);
    let (counts, _outcome) = count::objects_unthreaded(
        odb,
        &mut input,
        &progress::Discard,
        &AtomicBool::new(false),
        count::objects::ObjectExpansion::AsIs,
    )
    .expect("counting in-memory objects succeeds");
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

/// Phase 2: resolve, sort and encode `counts` into a pack stream, returning the pack size in bytes.
fn write_pack(odb: &MemoryOdb, counts: Vec<output::Count>) -> u64 {
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
    let mut group = c.benchmark_group("count-in-memory-objects");
    for &n in OBJECT_COUNTS {
        let (odb, object_ids) = memory_odb(n);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| count_objects_unthreaded(&odb, &object_ids));
        });
    }
    group.finish();
}

fn bench_write(c: &mut Criterion) {
    let mut group = c.benchmark_group("write-pack");
    for &n in OBJECT_COUNTS {
        let (odb, object_ids) = memory_odb(n);
        let counts = count_objects_unthreaded(&odb, &object_ids);
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

criterion_group!(benches, bench_count, bench_write);

fn main() {
    benches();
    Criterion::default().configure_from_args().final_summary();
}
