use a2a::{Artifact, Message, PartContent, Role, StreamResponse, Task, TaskState, TaskStatus};
use base64::Engine;

use crate::protocol::events::{A2aPartEventData, A2aPartKind, AgentStreamEvent, TextEventData, TipType, TipsEventData};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TurnOutcome {
    Continue,
    Completed,
    InputRequired,
    AuthRequired,
    Canceled,
    Failed,
    Rejected,
}

pub(crate) struct TranslatedEvent {
    pub events: Vec<AgentStreamEvent>,
    pub outcome: TurnOutcome,
    pub task: Option<Task>,
    pub task_id: Option<String>,
    pub context_id: Option<String>,
    pub artifact_snapshot: Option<serde_json::Value>,
}

pub(crate) fn translate_stream_response(response: StreamResponse) -> TranslatedEvent {
    match response {
        StreamResponse::Message(message) => TranslatedEvent {
            events: message_events(&message),
            outcome: TurnOutcome::Completed,
            task_id: message.task_id.clone(),
            context_id: message.context_id.clone(),
            task: None,
            artifact_snapshot: None,
        },
        StreamResponse::Task(task) => {
            let events = task_events(&task);
            let outcome = status_outcome(&task.status);
            let task_id = Some(task.id.clone());
            let context_id = Some(task.context_id.clone());
            let artifact_snapshot = task
                .artifacts
                .as_ref()
                .and_then(|artifacts| serde_json::to_value(artifacts).ok());
            TranslatedEvent {
                events,
                outcome,
                task: Some(task),
                task_id,
                context_id,
                artifact_snapshot,
            }
        }
        StreamResponse::StatusUpdate(update) => TranslatedEvent {
            events: update
                .status
                .message
                .as_ref()
                .map(message_events)
                .unwrap_or_default()
                .into_iter()
                .chain(status_tip(&update.status))
                .collect(),
            outcome: status_outcome(&update.status),
            task: None,
            task_id: Some(update.task_id),
            context_id: Some(update.context_id),
            artifact_snapshot: None,
        },
        StreamResponse::ArtifactUpdate(update) => TranslatedEvent {
            events: artifact_events(&update.artifact),
            outcome: TurnOutcome::Continue,
            task: None,
            task_id: Some(update.task_id),
            context_id: Some(update.context_id),
            artifact_snapshot: serde_json::to_value(&update.artifact).ok(),
        },
    }
}

pub(crate) fn translate_send_response(response: a2a::SendMessageResponse) -> TranslatedEvent {
    match response {
        a2a::SendMessageResponse::Task(task) => translate_stream_response(StreamResponse::Task(task)),
        a2a::SendMessageResponse::Message(message) => translate_stream_response(StreamResponse::Message(message)),
    }
}

pub(crate) fn translate_task(task: Task) -> TranslatedEvent {
    translate_stream_response(StreamResponse::Task(task))
}

fn task_events(task: &Task) -> Vec<AgentStreamEvent> {
    let mut events = task.status.message.as_ref().map(message_events).unwrap_or_default();
    if let Some(artifacts) = task.artifacts.as_ref() {
        for artifact in artifacts {
            events.extend(artifact_events(artifact));
        }
    }
    events.extend(status_tip(&task.status));
    events
}

fn message_events(message: &Message) -> Vec<AgentStreamEvent> {
    if message.role != Role::Agent {
        return Vec::new();
    }
    message
        .parts
        .iter()
        .filter_map(|part| match &part.content {
            PartContent::Text(content) if !content.is_empty() => Some(AgentStreamEvent::Text(TextEventData {
                content: content.clone(),
            })),
            PartContent::Url(url) => Some(AgentStreamEvent::A2aPart(A2aPartEventData {
                kind: A2aPartKind::Resource,
                artifact_id: None,
                name: None,
                description: None,
                url: Some(url.clone()),
                data: None,
                bytes_base64: None,
                byte_length: None,
                filename: part.filename.clone(),
                media_type: part.media_type.clone(),
            })),
            PartContent::Data(data) => Some(AgentStreamEvent::A2aPart(A2aPartEventData {
                kind: A2aPartKind::Data,
                artifact_id: None,
                name: None,
                description: None,
                url: None,
                data: Some(data.clone()),
                bytes_base64: None,
                byte_length: None,
                filename: part.filename.clone(),
                media_type: part.media_type.clone(),
            })),
            PartContent::Raw(bytes) => Some(AgentStreamEvent::A2aPart(A2aPartEventData {
                kind: A2aPartKind::InlineFile,
                artifact_id: None,
                name: None,
                description: None,
                url: None,
                data: None,
                bytes_base64: Some(base64::engine::general_purpose::STANDARD.encode(bytes)),
                byte_length: Some(bytes.len()),
                filename: part.filename.clone(),
                media_type: part.media_type.clone(),
            })),
            PartContent::Text(_) => None,
        })
        .collect()
}

fn artifact_events(artifact: &Artifact) -> Vec<AgentStreamEvent> {
    let mut events = Vec::new();
    for part in &artifact.parts {
        match &part.content {
            PartContent::Text(content) if !content.is_empty() => {
                events.push(AgentStreamEvent::Text(TextEventData {
                    content: content.clone(),
                }));
            }
            PartContent::Url(url) => events.push(AgentStreamEvent::A2aPart(A2aPartEventData {
                kind: A2aPartKind::Artifact,
                artifact_id: Some(artifact.artifact_id.clone()),
                name: artifact.name.clone(),
                description: artifact.description.clone(),
                url: Some(url.clone()),
                data: None,
                bytes_base64: None,
                byte_length: None,
                filename: part.filename.clone(),
                media_type: part.media_type.clone(),
            })),
            PartContent::Data(data) => events.push(AgentStreamEvent::A2aPart(A2aPartEventData {
                kind: A2aPartKind::Artifact,
                artifact_id: Some(artifact.artifact_id.clone()),
                name: artifact.name.clone(),
                description: artifact.description.clone(),
                url: None,
                data: Some(data.clone()),
                bytes_base64: None,
                byte_length: None,
                filename: part.filename.clone(),
                media_type: part.media_type.clone(),
            })),
            PartContent::Raw(bytes) => events.push(AgentStreamEvent::A2aPart(A2aPartEventData {
                kind: A2aPartKind::Artifact,
                artifact_id: Some(artifact.artifact_id.clone()),
                name: artifact.name.clone(),
                description: artifact.description.clone(),
                url: None,
                data: None,
                bytes_base64: Some(base64::engine::general_purpose::STANDARD.encode(bytes)),
                byte_length: Some(bytes.len()),
                filename: part.filename.clone(),
                media_type: part.media_type.clone(),
            })),
            PartContent::Text(_) => {}
        }
    }
    events
}

fn status_tip(status: &TaskStatus) -> Option<AgentStreamEvent> {
    match status.state {
        TaskState::InputRequired => Some(AgentStreamEvent::Tips(TipsEventData {
            content: "A2A Agent 需要更多输入，请回复后继续任务。".to_owned(),
            tip_type: TipType::Info,
            code: Some("a2a.input_required".to_owned()),
            params: None,
        })),
        TaskState::AuthRequired => Some(AgentStreamEvent::Tips(TipsEventData {
            content: "A2A Agent 要求补充或更新认证信息。".to_owned(),
            tip_type: TipType::Warning,
            code: Some("a2a.auth_required".to_owned()),
            params: None,
        })),
        TaskState::Canceled => Some(AgentStreamEvent::Tips(TipsEventData {
            content: "A2A 任务已取消。".to_owned(),
            tip_type: TipType::Info,
            code: Some("a2a.task_canceled".to_owned()),
            params: None,
        })),
        TaskState::Rejected => Some(AgentStreamEvent::Tips(TipsEventData {
            content: "A2A Agent 拒绝了该任务。".to_owned(),
            tip_type: TipType::Error,
            code: Some("a2a.task_rejected".to_owned()),
            params: None,
        })),
        TaskState::Failed => Some(AgentStreamEvent::Tips(TipsEventData {
            content: "A2A Agent 任务执行失败。".to_owned(),
            tip_type: TipType::Error,
            code: Some("a2a.task_failed".to_owned()),
            params: None,
        })),
        _ => None,
    }
}

fn status_outcome(status: &TaskStatus) -> TurnOutcome {
    match status.state {
        TaskState::Completed => TurnOutcome::Completed,
        TaskState::InputRequired => TurnOutcome::InputRequired,
        TaskState::AuthRequired => TurnOutcome::AuthRequired,
        TaskState::Canceled => TurnOutcome::Canceled,
        TaskState::Failed => TurnOutcome::Failed,
        TaskState::Rejected => TurnOutcome::Rejected,
        TaskState::Unspecified | TaskState::Submitted | TaskState::Working => TurnOutcome::Continue,
    }
}

pub(crate) fn task_state_name(outcome: &TurnOutcome) -> &'static str {
    match outcome {
        TurnOutcome::Continue => "working",
        TurnOutcome::Completed => "completed",
        TurnOutcome::InputRequired => "input_required",
        TurnOutcome::AuthRequired => "auth_required",
        TurnOutcome::Canceled => "canceled",
        TurnOutcome::Failed => "failed",
        TurnOutcome::Rejected => "rejected",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canceled_task_emits_an_observable_terminal_tip() {
        let translated = translate_task(Task {
            id: "remote-task".to_owned(),
            context_id: "remote-context".to_owned(),
            status: TaskStatus {
                state: TaskState::Canceled,
                message: None,
                timestamp: None,
            },
            artifacts: None,
            history: None,
            metadata: None,
        });

        assert_eq!(translated.outcome, TurnOutcome::Canceled);
        assert!(translated.events.iter().any(|event| {
            matches!(
                event,
                AgentStreamEvent::Tips(TipsEventData {
                    code: Some(code),
                    ..
                }) if code == "a2a.task_canceled"
            )
        }));
    }
}
