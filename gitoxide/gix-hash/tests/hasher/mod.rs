use gix_hash::Hasher;

#[test]
fn size_of_sha1() {
    assert_eq!(
        std::mem::size_of::<Hasher>(),
        if cfg!(target_arch = "x86") { 820 } else { 824 },
    );
}
