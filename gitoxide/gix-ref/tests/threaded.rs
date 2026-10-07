#[test]
fn is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<gix_ref::file::Store>();
}
