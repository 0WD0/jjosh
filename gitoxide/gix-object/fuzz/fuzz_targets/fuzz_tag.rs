#![no_main]

use libfuzzer_sys::fuzz_target;
use std::hint::black_box;

fuzz_target!(|tag: &[u8]| {
    _ = black_box(gix_object::TagRef::from_bytes(tag, gix_hash::Kind::Sha1));
    _ = black_box(gix_object::TagRefIter::from_bytes(tag, gix_hash::Kind::Sha1).count());
    _ = black_box(gix_object::TagRef::from_bytes(tag, gix_hash::Kind::Sha256));
    _ = black_box(gix_object::TagRefIter::from_bytes(tag, gix_hash::Kind::Sha256).count());
});
