use super::openai_responses_capabilities;

#[test]
fn responses_capabilities_advertise_streamed_reasoning_and_tool_calls_without_remote_resume() {
    let capabilities = openai_responses_capabilities();

    assert!(capabilities.supports_tool_stream);
    assert!(capabilities.supports_reasoning_effort);
    assert!(capabilities.can_surface_reasoning_stream());
    assert!(!capabilities.supports_response_handles);
    assert!(!capabilities.supports_background_tasks);
}
