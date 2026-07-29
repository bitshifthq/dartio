use ptyx::{CloseResult, FailureKind, Operation, OperationError};

#[test]
fn operation_errors_preserve_stable_native_context() {
    let error = OperationError::new(Operation::Write, FailureKind::NativeFailure, Some(32));

    assert_eq!(error.operation(), Operation::Write);
    assert_eq!(error.kind(), FailureKind::NativeFailure);
    assert_eq!(error.native_code(), Some(32));
    assert_eq!(
        error.to_string(),
        "write failed with native failure (native code 32)"
    );
}

#[test]
fn close_result_retains_every_failure_and_prioritizes_cleanup() {
    let input = OperationError::new(Operation::Write, FailureKind::Closed, None);
    let output = OperationError::new(Operation::Output, FailureKind::NativeFailure, Some(5));
    let cleanup = OperationError::new(Operation::Close, FailureKind::InfrastructureLost, None);
    let result = CloseResult {
        input_failure: Some(input),
        output_failure: Some(output),
        cleanup_failure: Some(cleanup),
    };

    assert!(!result.is_success());
    assert_eq!(result.primary_failure(), Some(cleanup));
    assert_eq!(result.input_failure, Some(input));
    assert_eq!(result.output_failure, Some(output));
}
