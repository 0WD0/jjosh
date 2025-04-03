use gix_hash::{Hasher, ObjectId};

#[test]
fn size_of_hasher() {
    assert_eq!(
        std::mem::size_of::<Hasher>(),
        if cfg!(target_arch = "x86") { 820 } else { 824 },
        "The size of this type may be relevant when hashing millions of objects,\
        and shouldn't change unnoticed. The DetectionState alone clocks in at 724 bytes."
    );
}

#[test]
fn size_of_try_finalize_return_type() {
    assert_eq!(
        std::mem::size_of::<Result<ObjectId, gix_hash::hasher::Error>>(),
        21,
        "The size of the return value is just 1 byte larger than just returning the object hash itself"
    );
}
