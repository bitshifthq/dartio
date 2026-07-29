#[cfg(any(target_os = "linux", target_os = "macos"))]
use ptyx::{Runtime, RuntimeError};

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

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn unix_runtime_requires_an_explicit_broker() {
    let Err(error) = Runtime::builder().build() else {
        panic!("runtime unexpectedly accepted no Unix broker");
    };

    assert!(matches!(error, RuntimeError::MissingBroker));
}
