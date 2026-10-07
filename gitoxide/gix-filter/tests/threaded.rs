#[test]
fn is_send_with_parallel_enabled() {
    fn assert_send<T: Send>() {}
    assert_send::<gix_filter::Pipeline>();
}
