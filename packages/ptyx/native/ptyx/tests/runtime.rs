use ptyx::Runtime;

#[test]
fn runtime_and_session_controls_are_thread_safe() {
    fn assert_send_sync<T: Send + Sync>() {}

    assert_send_sync::<ptyx::Runtime>();
    assert_send_sync::<ptyx::Session>();
    assert_send_sync::<ptyx::Events>();

    fn assert_event_stream<T: futures_core::Stream<Item = Result<ptyx::Event, ptyx::RecvError>>>() {
    }
    assert_event_stream::<ptyx::Events>();
}

#[test]
fn runtime_uses_its_packaged_platform_support_by_default() {
    Runtime::new().expect("create runtime with packaged platform support");
}
