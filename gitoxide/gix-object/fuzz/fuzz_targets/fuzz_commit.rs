#![no_main]
use libfuzzer_sys::fuzz_target;
use std::hint::black_box;

fuzz_target!(|commit: &[u8]| {
    _ = black_box(gix_object::CommitRef::from_bytes(commit, gix_hash::Kind::Sha1));
    _ = black_box(gix_object::CommitRefIter::from_bytes(commit, gix_hash::Kind::Sha1)).count();
    _ = black_box(gix_object::CommitRef::from_bytes(commit, gix_hash::Kind::Sha256));
    _ = black_box(gix_object::CommitRefIter::from_bytes(commit, gix_hash::Kind::Sha256)).count();
});
