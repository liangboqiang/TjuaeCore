use a2a::{PartContent, Role, SendMessageResponse, TaskState};

#[test]
fn v1_message_fixture_roundtrips_text_and_structured_data() {
    let fixture = serde_json::json!({
        "messageId": "message-1",
        "contextId": "context-1",
        "role": "ROLE_USER",
        "parts": [
            {"text": "book a trip", "mediaType": "text/plain"},
            {"data": {"destination": "Tianjin", "passengers": 2}, "mediaType": "application/json"}
        ]
    });

    let message: a2a::Message = serde_json::from_value(fixture).expect("official v1 message shape");
    assert_eq!(message.role, Role::User);
    assert!(matches!(message.parts[1].content, PartContent::Data(_)));

    let roundtrip: a2a::Message =
        serde_json::from_value(serde_json::to_value(message).unwrap()).expect("message roundtrip");
    assert_eq!(roundtrip.context_id.as_deref(), Some("context-1"));
}

#[test]
fn v1_task_and_artifact_fixture_roundtrips() {
    let fixture = serde_json::json!({
        "id": "task-1",
        "contextId": "context-1",
        "status": {
            "state": "TASK_STATE_COMPLETED",
            "message": {
                "messageId": "message-2",
                "role": "ROLE_AGENT",
                "parts": [{"text": "done"}]
            }
        },
        "artifacts": [{
            "artifactId": "artifact-1",
            "name": "result.json",
            "parts": [{
                "data": {"ok": true},
                "filename": "result.json",
                "mediaType": "application/json"
            }]
        }]
    });

    let task: a2a::Task = serde_json::from_value(fixture).expect("official v1 task shape");
    assert_eq!(task.status.state, TaskState::Completed);
    assert_eq!(task.artifacts.as_ref().unwrap()[0].artifact_id, "artifact-1");

    let response = SendMessageResponse::Task(task);
    let roundtrip: SendMessageResponse =
        serde_json::from_value(serde_json::to_value(response).unwrap()).expect("task response roundtrip");
    assert!(matches!(roundtrip, SendMessageResponse::Task(_)));
}

#[test]
fn v1_direct_message_response_fixture_deserializes() {
    let fixture = serde_json::json!({
        "message": {
            "messageId": "message-direct",
            "role": "ROLE_AGENT",
            "parts": [{"text": "hello"}]
        }
    });
    let response: SendMessageResponse = serde_json::from_value(fixture).expect("direct message response");
    assert!(matches!(response, SendMessageResponse::Message(_)));
}
