use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use a2a::{Message, Part, Role, SendMessageConfiguration, SendMessageRequest};
use tjuaeui_common::{AgentKillReason, AgentType, ConversationStatus, TimestampMs};
use tjuaeui_db::{A2aTaskRow, IA2aRepository, UpsertA2aTaskParams};
use tokio::sync::{Mutex, Notify, broadcast};
use tracing::warn;

use crate::agent_runtime::AgentRuntime;
use crate::agent_task::IAgentTask;
use crate::error::AgentError;
use crate::protocol::events::{AgentStreamEvent, StartEventData};
use crate::protocol::send_error::AgentSendError;
use crate::runtime_assets::RuntimeAssetLoadReceipt;
use crate::types::SendMessageData;

use super::client::{IA2aClient, IA2aEventStream};
use super::translate::{
    TranslatedEvent, TurnOutcome, task_state_name, translate_send_response, translate_stream_response, translate_task,
};

const MAX_ATTACHMENT_BYTES: u64 = 10 * 1024 * 1024;
const MAX_TOTAL_ATTACHMENT_BYTES: u64 = 25 * 1024 * 1024;
const MAX_POLL_ATTEMPTS: usize = 300;

#[derive(Debug, Clone, Default)]
struct TaskState {
    local_id: Option<String>,
    remote_task_id: Option<String>,
    task_continuable: bool,
    context_id: Option<String>,
    last_event_id: Option<String>,
    artifact_snapshot: Option<serde_json::Value>,
    last_task_snapshot: Option<serde_json::Value>,
}

pub struct A2aAgentManager {
    runtime: AgentRuntime,
    agent_id: String,
    client: Arc<dyn IA2aClient>,
    repo: Arc<dyn IA2aRepository>,
    interface_snapshot_json: String,
    accepted_output_modes: Vec<String>,
    supports_streaming: bool,
    preset_context: Option<String>,
    runtime_asset_receipt: Option<RuntimeAssetLoadReceipt>,
    state: Mutex<TaskState>,
    turn_guard: Mutex<()>,
    cancel_guard: Mutex<()>,
    cancel_notify: Notify,
    cancel_dispatched: AtomicBool,
    recovery_claimed: AtomicBool,
    killed: AtomicBool,
}

impl A2aAgentManager {
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn new(
        conversation_id: String,
        workspace: String,
        agent_id: String,
        client: Arc<dyn IA2aClient>,
        repo: Arc<dyn IA2aRepository>,
        interface_snapshot_json: String,
        accepted_output_modes: Vec<String>,
        supports_streaming: bool,
        preset_context: Option<String>,
    ) -> Result<Self, AgentError> {
        Self::new_with_runtime_asset_receipt(
            conversation_id,
            workspace,
            agent_id,
            client,
            repo,
            interface_snapshot_json,
            accepted_output_modes,
            supports_streaming,
            preset_context,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn new_with_runtime_asset_receipt(
        conversation_id: String,
        workspace: String,
        agent_id: String,
        client: Arc<dyn IA2aClient>,
        repo: Arc<dyn IA2aRepository>,
        interface_snapshot_json: String,
        accepted_output_modes: Vec<String>,
        supports_streaming: bool,
        preset_context: Option<String>,
        runtime_asset_receipt: Option<RuntimeAssetLoadReceipt>,
    ) -> Result<Self, AgentError> {
        let persisted = repo
            .find_task_by_conversation(&conversation_id)
            .await
            .map_err(db_error)?;
        Ok(Self {
            runtime: AgentRuntime::new(conversation_id, workspace, 256),
            agent_id,
            client,
            repo,
            interface_snapshot_json,
            accepted_output_modes,
            supports_streaming,
            preset_context,
            runtime_asset_receipt,
            state: Mutex::new(task_state_from_row(persisted)),
            turn_guard: Mutex::new(()),
            cancel_guard: Mutex::new(()),
            cancel_notify: Notify::new(),
            cancel_dispatched: AtomicBool::new(false),
            recovery_claimed: AtomicBool::new(false),
            killed: AtomicBool::new(false),
        })
    }

    pub(crate) fn runtime_asset_receipt(&self) -> Option<RuntimeAssetLoadReceipt> {
        self.runtime_asset_receipt.clone()
    }

    pub async fn claim_pending_recovery(&self) -> bool {
        let state = self.state.lock().await;
        let pending = state.task_continuable && state.remote_task_id.is_some();
        drop(state);
        pending
            && self
                .recovery_claimed
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
    }

    pub fn abandon_pending_recovery(&self) {
        self.recovery_claimed.store(false, Ordering::Release);
    }

    pub async fn resume_pending_task(&self) -> Result<(), AgentSendError> {
        let _turn = self.turn_guard.lock().await;
        if self.killed.load(Ordering::Relaxed) {
            self.recovery_claimed.store(false, Ordering::Release);
            return Err(AgentSendError::from_agent_error(AgentError::conflict(
                "A2A 会话已经终止",
            )));
        }
        let task_id = {
            let state = self.state.lock().await;
            state.task_continuable.then(|| state.remote_task_id.clone()).flatten()
        };
        let Some(task_id) = task_id else {
            self.recovery_claimed.store(false, Ordering::Release);
            return Ok(());
        };

        self.cancel_dispatched.store(false, Ordering::Release);
        self.runtime.bump_activity();
        self.runtime.reset_for_new_turn(ConversationStatus::Running);
        self.runtime.emit(AgentStreamEvent::Start(StartEventData {
            session_id: Some(task_id.clone()),
        }));
        let result = self.resume_or_poll(&task_id).await;
        self.runtime.bump_activity();
        self.recovery_claimed.store(false, Ordering::Release);
        match result {
            Ok(()) => {
                let session_id = self.state.lock().await.remote_task_id.clone();
                self.runtime.emit_finish(session_id);
                Ok(())
            }
            Err(error) => {
                let send_error = AgentSendError::from_agent_error(error);
                self.runtime.emit_error_data(send_error.stream_error().clone());
                Err(send_error)
            }
        }
    }

    async fn build_request(&self, data: &SendMessageData) -> Result<SendMessageRequest, AgentError> {
        let text = self
            .preset_context
            .as_deref()
            .map(|rules| {
                format!(
                    "<assistant_instructions>\n{rules}\n</assistant_instructions>\n\n<user_message>\n{}\n</user_message>",
                    data.content
                )
            })
            .unwrap_or_else(|| data.content.clone());
        let mut parts = vec![Part::text(text)];
        let mut total = 0u64;
        for path in &data.files {
            let metadata = tokio::fs::metadata(path)
                .await
                .map_err(|_| AgentError::bad_request("无法读取 A2A 附件"))?;
            if !metadata.is_file() {
                return Err(AgentError::bad_request("A2A 附件必须是普通文件"));
            }
            if metadata.len() > MAX_ATTACHMENT_BYTES
                || total.saturating_add(metadata.len()) > MAX_TOTAL_ATTACHMENT_BYTES
            {
                return Err(AgentError::bad_request("A2A 附件超过大小限制"));
            }
            total += metadata.len();
            let bytes = tokio::fs::read(path)
                .await
                .map_err(|_| AgentError::bad_request("无法读取 A2A 附件内容"))?;
            let filename = Path::new(path)
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("attachment")
                .to_owned();
            let media_type = mime_guess::from_path(path)
                .first_or_octet_stream()
                .essence_str()
                .to_owned();
            let part = if media_type == "application/json" {
                let value = serde_json::from_slice(&bytes)
                    .map_err(|_| AgentError::bad_request("A2A JSON 附件不是有效 JSON"))?;
                Part::data(value)
            } else {
                Part::raw(bytes)
            };
            parts.push(part.with_filename(filename).with_media_type(media_type));
        }
        let state = self.state.lock().await;
        let mut message = Message::new(Role::User, parts);
        message.message_id = data.msg_id.clone();
        // A terminal A2A task is immutable. Follow-up turns continue the same
        // context but must start a fresh task, while input/auth-required tasks
        // keep their task id so the remote agent can resume them.
        message.task_id = state.task_continuable.then(|| state.remote_task_id.clone()).flatten();
        message.context_id = state.context_id.clone();
        Ok(SendMessageRequest {
            message,
            configuration: Some(SendMessageConfiguration {
                accepted_output_modes: (!self.accepted_output_modes.is_empty())
                    .then(|| self.accepted_output_modes.clone()),
                task_push_notification_config: None,
                history_length: Some(20),
                return_immediately: Some(false),
            }),
            metadata: None,
            tenant: None,
        })
    }

    async fn run_turn(&self, request: &SendMessageRequest) -> Result<(), AgentError> {
        if self.supports_streaming {
            match self.client.send_streaming_message(request).await {
                Ok(mut stream) => return self.consume_stream(stream.as_mut()).await,
                Err(error) => {
                    // The retry carries the exact same Message ID. A2A servers
                    // can therefore deduplicate an accepted streaming request
                    // whose response failed before the first SSE event.
                    warn!(%error, message_id = %request.message.message_id, "A2A streaming unavailable; retrying the same message through the non-streaming endpoint");
                }
            }
        }
        let response = self.client.send_message(request).await?;
        let translated = translate_send_response(response);
        let outcome = self.apply_translated(translated, None).await?;
        self.finish_or_poll(outcome).await
    }

    async fn consume_stream(&self, stream: &mut dyn IA2aEventStream) -> Result<(), AgentError> {
        let mut received_event = false;
        loop {
            let next = tokio::select! {
                next = stream.next() => {
                    match next {
                        Ok(value) => value,
                        Err(error) => {
                            let task_id = self.state.lock().await.remote_task_id.clone();
                            if let Some(task_id) = task_id {
                                warn!(%error, %task_id, "A2A event stream interrupted; recovering by subscription/polling");
                                return self.resume_or_poll(&task_id).await;
                            }
                            return Err(error);
                        }
                    }
                },
                _ = self.cancel_notify.notified() => {
                    return self.cancel_remote_task().await;
                }
            };
            let Some(item) = next else {
                break;
            };
            received_event = true;
            let outcome = self
                .apply_translated(translate_stream_response(item.event), item.event_id)
                .await?;
            if outcome != TurnOutcome::Continue {
                return self.finish_or_poll(outcome).await;
            }
        }

        let task_id = self.state.lock().await.remote_task_id.clone();
        if let Some(task_id) = task_id {
            return self.resume_or_poll(&task_id).await;
        }
        if received_event {
            Ok(())
        } else {
            Err(AgentError::bad_gateway("A2A 事件流在返回任何事件前已关闭"))
        }
    }

    async fn resume_or_poll(&self, task_id: &str) -> Result<(), AgentError> {
        let last_event_id = self.state.lock().await.last_event_id.clone();
        match self.client.subscribe_to_task(task_id, last_event_id.as_deref()).await {
            Ok(mut subscription) => loop {
                let next = tokio::select! {
                    next = subscription.next() => {
                        match next {
                            Ok(value) => value,
                            Err(error) => {
                                warn!(%error, %task_id, "A2A task subscription interrupted; using polling");
                                break;
                            }
                        }
                    },
                    _ = self.cancel_notify.notified() => {
                        return self.cancel_remote_task().await;
                    }
                };
                let Some(item) = next else {
                    break;
                };
                let outcome = self
                    .apply_translated(translate_stream_response(item.event), item.event_id)
                    .await?;
                if outcome != TurnOutcome::Continue {
                    return self.finish_or_poll(outcome).await;
                }
            },
            Err(error) => {
                warn!(%error, %task_id, "A2A task resubscribe unavailable; using polling");
            }
        }
        self.poll_task(task_id).await
    }

    async fn finish_or_poll(&self, outcome: TurnOutcome) -> Result<(), AgentError> {
        match outcome {
            TurnOutcome::Continue => {
                let task_id = self
                    .state
                    .lock()
                    .await
                    .remote_task_id
                    .clone()
                    .ok_or_else(|| AgentError::bad_gateway("A2A 响应未提供可继续跟踪的 task_id"))?;
                self.poll_task(&task_id).await
            }
            terminal => Self::finish_terminal(terminal),
        }
    }

    fn finish_terminal(outcome: TurnOutcome) -> Result<(), AgentError> {
        match outcome {
            TurnOutcome::Completed | TurnOutcome::InputRequired | TurnOutcome::AuthRequired | TurnOutcome::Canceled => {
                Ok(())
            }
            TurnOutcome::Failed => Err(AgentError::bad_gateway("A2A 任务执行失败")),
            TurnOutcome::Rejected => Err(AgentError::bad_request("A2A Agent 拒绝了该任务")),
            TurnOutcome::Continue => Err(AgentError::internal("A2A 非终态被错误地交给终态处理器")),
        }
    }

    async fn poll_task(&self, task_id: &str) -> Result<(), AgentError> {
        for _ in 0..MAX_POLL_ATTEMPTS {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                _ = self.cancel_notify.notified() => return self.cancel_remote_task().await,
            }
            let outcome = self
                .apply_translated(translate_task(self.client.get_task(task_id).await?), None)
                .await?;
            if outcome != TurnOutcome::Continue {
                return Self::finish_terminal(outcome);
            }
        }
        Err(AgentError::timeout("A2A 任务轮询超时"))
    }

    async fn apply_translated(
        &self,
        translated: TranslatedEvent,
        event_id: Option<String>,
    ) -> Result<TurnOutcome, AgentError> {
        let task_snapshot = translated
            .task
            .as_ref()
            .and_then(|task| serde_json::to_value(task).ok());
        let mut state = self.state.lock().await;
        let duplicate_task_snapshot =
            task_snapshot.is_some() && task_snapshot.as_ref() == state.last_task_snapshot.as_ref();
        if task_snapshot.is_some() {
            state.last_task_snapshot = task_snapshot;
        }
        let duplicate_artifact_delta = translated.task.is_none()
            && translated
                .artifact_snapshot
                .as_ref()
                .is_some_and(|artifact| artifact_snapshot_contains(state.artifact_snapshot.as_ref(), artifact));
        if !duplicate_task_snapshot && !duplicate_artifact_delta {
            for event in translated.events {
                self.runtime.emit(event);
            }
        }
        if let Some(task_id) = translated.task_id {
            state.remote_task_id = Some(task_id);
        }
        state.task_continuable = matches!(
            translated.outcome,
            TurnOutcome::Continue | TurnOutcome::InputRequired | TurnOutcome::AuthRequired
        );
        if let Some(context_id) = translated.context_id {
            state.context_id = Some(context_id);
        }
        if event_id.is_some() {
            state.last_event_id = event_id;
        }
        if let Some(artifact_snapshot) = translated.artifact_snapshot {
            if translated.task.is_some() {
                state.artifact_snapshot = Some(artifact_snapshot);
            } else {
                merge_artifact_snapshot(&mut state.artifact_snapshot, artifact_snapshot);
            }
        }
        let artifact_snapshot_json = state.artifact_snapshot.as_ref().map(serde_json::Value::to_string);
        let row = self
            .repo
            .upsert_task(UpsertA2aTaskParams {
                id: state.local_id.as_deref(),
                conversation_id: self.runtime.conversation_id(),
                agent_id: &self.agent_id,
                remote_task_id: state.remote_task_id.as_deref(),
                context_id: state.context_id.as_deref(),
                state: task_state_name(&translated.outcome),
                interface_snapshot_json: &self.interface_snapshot_json,
                last_event_id: state.last_event_id.as_deref(),
                artifact_snapshot_json: artifact_snapshot_json.as_deref(),
                push_config_json: None,
            })
            .await
            .map_err(db_error)?;
        state.local_id = Some(row.id);
        Ok(translated.outcome)
    }

    async fn cancel_remote_task(&self) -> Result<(), AgentError> {
        // Both the HTTP cancel handler and the in-flight send loop can observe
        // the same cancellation. Serialize them so the losing caller waits
        // until the remote terminal state has been translated and emitted;
        // otherwise it could emit Finish first and make the relay drop the
        // subsequent "canceled" tip.
        let _cancel = self.cancel_guard.lock().await;
        if self
            .cancel_dispatched
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(());
        }
        let state = self.state.lock().await;
        let task_id = state.task_continuable.then(|| state.remote_task_id.clone()).flatten();
        drop(state);
        if let Some(task_id) = task_id {
            let task = match self.client.cancel_task(&task_id).await {
                Ok(task) => task,
                Err(error) => {
                    self.cancel_dispatched.store(false, Ordering::Release);
                    return Err(error);
                }
            };
            let _ = self.apply_translated(translate_task(task), None).await?;
        }
        Ok(())
    }

    pub fn kill_and_wait(
        self: &Arc<Self>,
        reason: Option<AgentKillReason>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let _ = self.kill(reason);
        let manager = self.clone();
        Box::pin(async move {
            if let Err(error) = manager.cancel_remote_task().await {
                warn!(agent_id = %manager.agent_id, %error, "failed to cancel remote A2A task during teardown");
            }
        })
    }
}

#[async_trait::async_trait]
impl IAgentTask for A2aAgentManager {
    fn agent_type(&self) -> AgentType {
        AgentType::A2a
    }

    fn conversation_id(&self) -> &str {
        self.runtime.conversation_id()
    }

    fn workspace(&self) -> &str {
        self.runtime.workspace()
    }

    fn status(&self) -> Option<ConversationStatus> {
        self.runtime.status()
    }

    fn last_activity_at(&self) -> TimestampMs {
        self.runtime.last_activity_at()
    }

    fn subscribe(&self) -> broadcast::Receiver<AgentStreamEvent> {
        self.runtime.subscribe()
    }

    async fn send_message(&self, data: SendMessageData) -> Result<(), AgentSendError> {
        let _turn = self.turn_guard.lock().await;
        if self.killed.load(Ordering::Relaxed) {
            return Err(AgentSendError::from_agent_error(AgentError::conflict(
                "A2A 会话已经终止",
            )));
        }
        self.cancel_dispatched.store(false, Ordering::Release);
        self.runtime.bump_activity();
        self.runtime.reset_for_new_turn(ConversationStatus::Running);
        let session_id = self.state.lock().await.remote_task_id.clone();
        self.runtime
            .emit(AgentStreamEvent::Start(StartEventData { session_id }));
        let result = match self.build_request(&data).await {
            Ok(request) => self.run_turn(&request).await,
            Err(error) => Err(error),
        };
        self.runtime.bump_activity();
        match result {
            Ok(()) => {
                let session_id = self.state.lock().await.remote_task_id.clone();
                self.runtime.emit_finish(session_id);
                Ok(())
            }
            Err(error) => {
                let send_error = AgentSendError::from_agent_error(error);
                self.runtime.emit_error_data(send_error.stream_error().clone());
                Err(send_error)
            }
        }
    }

    async fn cancel(&self) -> Result<(), AgentError> {
        self.cancel_notify.notify_one();
        self.cancel_remote_task().await
    }

    fn kill(&self, _reason: Option<AgentKillReason>) -> Result<(), AgentError> {
        self.killed.store(true, Ordering::Relaxed);
        self.cancel_notify.notify_one();
        Ok(())
    }
}

fn task_state_from_row(row: Option<A2aTaskRow>) -> TaskState {
    row.map(|row| TaskState {
        local_id: Some(row.id),
        remote_task_id: row.remote_task_id,
        task_continuable: matches!(row.state.as_str(), "working" | "input_required" | "auth_required"),
        context_id: row.context_id,
        last_event_id: row.last_event_id,
        artifact_snapshot: row
            .artifact_snapshot_json
            .as_deref()
            .and_then(|value| serde_json::from_str(value).ok()),
        last_task_snapshot: None,
    })
    .unwrap_or_default()
}

fn artifact_snapshot_contains(current: Option<&serde_json::Value>, candidate: &serde_json::Value) -> bool {
    let Some(candidate_id) = candidate.get("artifactId").and_then(serde_json::Value::as_str) else {
        return current == Some(candidate);
    };
    current.and_then(serde_json::Value::as_array).is_some_and(|artifacts| {
        artifacts.iter().any(|artifact| {
            artifact.get("artifactId").and_then(serde_json::Value::as_str) == Some(candidate_id)
                && artifact == candidate
        })
    })
}

fn merge_artifact_snapshot(current: &mut Option<serde_json::Value>, candidate: serde_json::Value) {
    let candidate_id = candidate
        .get("artifactId")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let artifacts = current.get_or_insert_with(|| serde_json::Value::Array(Vec::new()));
    if !artifacts.is_array() {
        *artifacts = serde_json::Value::Array(Vec::new());
    }
    let values = artifacts
        .as_array_mut()
        .expect("artifact snapshot was normalized to an array");
    if let Some(candidate_id) = candidate_id
        && let Some(existing) = values.iter_mut().find(|artifact| {
            artifact.get("artifactId").and_then(serde_json::Value::as_str) == Some(candidate_id.as_str())
        })
    {
        *existing = candidate;
    } else {
        values.push(candidate);
    }
}

fn db_error(error: impl std::fmt::Display) -> AgentError {
    AgentError::internal(format!("A2A 任务持久化失败：{error}"))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tjuaeui_db::{IA2aRepository, SqliteA2aRepository, UpsertA2aAgentProfileParams, init_database_memory};

    use super::*;
    use crate::protocol::events::TipsEventData;

    struct MockClient {
        send_state: a2a::TaskState,
        get_calls: AtomicUsize,
        cancel_calls: AtomicUsize,
        cancel_delay: Duration,
        requests: Mutex<Vec<a2a::SendMessageRequest>>,
    }

    impl MockClient {
        fn new(send_state: a2a::TaskState) -> Self {
            Self {
                send_state,
                get_calls: AtomicUsize::new(0),
                cancel_calls: AtomicUsize::new(0),
                cancel_delay: Duration::ZERO,
                requests: Mutex::new(Vec::new()),
            }
        }

        fn with_cancel_delay(mut self, delay: Duration) -> Self {
            self.cancel_delay = delay;
            self
        }

        fn task(state: a2a::TaskState) -> a2a::Task {
            a2a::Task {
                id: "remote-poll-task".to_owned(),
                context_id: "remote-poll-context".to_owned(),
                status: a2a::TaskStatus {
                    state,
                    message: Some(a2a::Message::new(
                        a2a::Role::Agent,
                        vec![a2a::Part::text("remote result")],
                    )),
                    timestamp: None,
                },
                artifacts: None,
                history: None,
                metadata: None,
            }
        }
    }

    #[async_trait::async_trait]
    impl IA2aClient for MockClient {
        async fn send_message(
            &self,
            request: &a2a::SendMessageRequest,
        ) -> Result<a2a::SendMessageResponse, AgentError> {
            self.requests.lock().await.push(request.clone());
            Ok(a2a::SendMessageResponse::Task(Self::task(self.send_state.clone())))
        }

        async fn send_streaming_message(
            &self,
            _request: &a2a::SendMessageRequest,
        ) -> Result<Box<dyn IA2aEventStream>, AgentError> {
            Err(AgentError::bad_gateway("stream disabled in test"))
        }

        async fn get_task(&self, _task_id: &str) -> Result<a2a::Task, AgentError> {
            self.get_calls.fetch_add(1, Ordering::SeqCst);
            Ok(Self::task(a2a::TaskState::Completed))
        }

        async fn list_tasks(&self, _request: &a2a::ListTasksRequest) -> Result<a2a::ListTasksResponse, AgentError> {
            Ok(a2a::ListTasksResponse {
                tasks: Vec::new(),
                next_page_token: String::new(),
                page_size: 0,
                total_size: 0,
            })
        }

        async fn subscribe_to_task(
            &self,
            _task_id: &str,
            _last_event_id: Option<&str>,
        ) -> Result<Box<dyn IA2aEventStream>, AgentError> {
            Err(AgentError::bad_gateway("subscription disabled in test"))
        }

        async fn cancel_task(&self, _task_id: &str) -> Result<a2a::Task, AgentError> {
            self.cancel_calls.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(self.cancel_delay).await;
            Ok(Self::task(a2a::TaskState::Canceled))
        }

        async fn get_extended_agent_card(&self) -> Result<a2a::AgentCard, AgentError> {
            Err(AgentError::bad_gateway("unused"))
        }

        async fn create_push_config(
            &self,
            _config: &a2a::TaskPushNotificationConfig,
        ) -> Result<a2a::TaskPushNotificationConfig, AgentError> {
            Err(AgentError::bad_gateway("unused"))
        }

        async fn get_push_config(
            &self,
            _request: &a2a::GetTaskPushNotificationConfigRequest,
        ) -> Result<a2a::TaskPushNotificationConfig, AgentError> {
            Err(AgentError::bad_gateway("unused"))
        }

        async fn list_push_configs(
            &self,
            _request: &a2a::ListTaskPushNotificationConfigsRequest,
        ) -> Result<a2a::ListTaskPushNotificationConfigsResponse, AgentError> {
            Err(AgentError::bad_gateway("unused"))
        }

        async fn delete_push_config(
            &self,
            _request: &a2a::DeleteTaskPushNotificationConfigRequest,
        ) -> Result<(), AgentError> {
            Err(AgentError::bad_gateway("unused"))
        }
    }

    async fn setup_repo() -> Arc<SqliteA2aRepository> {
        let database = init_database_memory().await.unwrap();
        let now = tjuaeui_common::now_ms();
        sqlx::query(
            "INSERT INTO agent_metadata (
                id, name, agent_type, agent_source, enabled, sort_order, created_at, updated_at
             ) VALUES (?, 'Poll Test', 'a2a', 'custom', 1, 5000, ?, ?)",
        )
        .bind("a2a-poll-test")
        .bind(now)
        .bind(now)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO conversations (
                id, user_id, name, type, status, created_at, updated_at
             ) VALUES (?, 'system_default_user', 'A2A Poll', 'a2a', 'running', ?, ?)",
        )
        .bind("conversation-poll-test")
        .bind(now)
        .bind(now)
        .execute(database.pool())
        .await
        .unwrap();
        let repo = Arc::new(SqliteA2aRepository::new(database.pool().clone()));
        repo.upsert_profile(UpsertA2aAgentProfileParams {
            agent_id: "a2a-poll-test",
            card_url: "https://agent.example/.well-known/agent-card.json",
            base_url: "https://agent.example",
            display_name: Some("Poll Test"),
            allow_insecure: false,
            allow_private_network: false,
            compatibility_mode: "v1",
            raw_card_json: None,
            normalized_card_json: None,
            extended_card_json: None,
            protocol_version: Some("1.0"),
            selected_binding: Some("json_rpc"),
            selected_interface_url: Some("https://agent.example/a2a"),
            credential_ref: None,
            credential_refs_json: "[]",
            selected_tenant: None,
            etag: None,
            last_modified: None,
            cache_expires_at: None,
            fetched_at: Some(now),
            card_hash: None,
            signature_status: "unchecked",
            trust_status: "untrusted",
            trusted_origin: None,
        })
        .await
        .unwrap();
        repo
    }

    async fn manager(client: Arc<MockClient>, repo: Arc<SqliteA2aRepository>) -> A2aAgentManager {
        A2aAgentManager::new(
            "conversation-poll-test".to_owned(),
            ".".to_owned(),
            "a2a-poll-test".to_owned(),
            client,
            repo,
            r#"{"binding":"json_rpc"}"#.to_owned(),
            vec!["text/plain".to_owned()],
            false,
            None,
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn non_streaming_poll_persists_terminal_state_and_restart_resumes_task() {
        let repo = setup_repo().await;
        let first_client = Arc::new(MockClient::new(a2a::TaskState::Working));
        let first_manager = manager(first_client.clone(), repo.clone()).await;
        first_manager
            .send_message(SendMessageData {
                content: "first turn".to_owned(),
                msg_id: "message-1".to_owned(),
                turn_id: None,
                files: Vec::new(),
                inject_skills: Vec::new(),
            })
            .await
            .unwrap();
        assert_eq!(first_client.get_calls.load(Ordering::SeqCst), 1);
        let persisted = repo
            .find_task_by_conversation("conversation-poll-test")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(persisted.state, "completed");
        assert_eq!(persisted.remote_task_id.as_deref(), Some("remote-poll-task"));

        let resumed_client = Arc::new(MockClient::new(a2a::TaskState::Completed));
        let resumed_manager = manager(resumed_client.clone(), repo.clone()).await;
        resumed_manager
            .send_message(SendMessageData {
                content: "second turn".to_owned(),
                msg_id: "message-2".to_owned(),
                turn_id: None,
                files: Vec::new(),
                inject_skills: Vec::new(),
            })
            .await
            .unwrap();
        let requests = resumed_client.requests.lock().await;
        assert_eq!(requests[0].message.task_id, None);
        assert_eq!(requests[0].message.context_id.as_deref(), Some("remote-poll-context"));
    }

    #[tokio::test]
    async fn input_required_follow_up_keeps_the_remote_task_id() {
        let repo = setup_repo().await;
        let client = Arc::new(MockClient::new(a2a::TaskState::InputRequired));
        let manager = manager(client.clone(), repo).await;

        for (content, msg_id) in [("first turn", "message-1"), ("follow up", "message-2")] {
            manager
                .send_message(SendMessageData {
                    content: content.to_owned(),
                    msg_id: msg_id.to_owned(),
                    turn_id: None,
                    files: Vec::new(),
                    inject_skills: Vec::new(),
                })
                .await
                .unwrap();
        }

        let requests = client.requests.lock().await;
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].message.task_id, None);
        assert_eq!(requests[1].message.task_id.as_deref(), Some("remote-poll-task"));
        assert_eq!(requests[1].message.context_id.as_deref(), Some("remote-poll-context"));
    }

    #[tokio::test]
    async fn concurrent_cancel_paths_emit_canceled_tip_before_finish() {
        let repo = setup_repo().await;
        let client = Arc::new(MockClient::new(a2a::TaskState::Working).with_cancel_delay(Duration::from_millis(75)));
        let manager = Arc::new(manager(client.clone(), repo).await);
        let mut events = manager.subscribe();
        let send_manager = manager.clone();
        let send = tokio::spawn(async move {
            send_manager
                .send_message(SendMessageData {
                    content: "cancel me".to_owned(),
                    msg_id: "message-cancel".to_owned(),
                    turn_id: None,
                    files: Vec::new(),
                    inject_skills: Vec::new(),
                })
                .await
        });

        tokio::time::timeout(Duration::from_secs(1), async {
            while client.requests.lock().await.is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("send request should start");
        manager.cancel().await.unwrap();
        send.await.unwrap().unwrap();

        let received: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).collect();
        let canceled_tip = received
            .iter()
            .position(|event| {
                matches!(
                    event,
                    AgentStreamEvent::Tips(TipsEventData {
                        code: Some(code),
                        ..
                    }) if code == "a2a.task_canceled"
                )
            })
            .expect("canceled tip");
        let finish = received
            .iter()
            .position(|event| matches!(event, AgentStreamEvent::Finish(_)))
            .expect("finish event");
        assert!(canceled_tip < finish);
        assert_eq!(client.cancel_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn pending_recovery_resumes_without_sending_a_new_user_message() {
        let repo = setup_repo().await;
        repo.upsert_task(UpsertA2aTaskParams {
            id: None,
            conversation_id: "conversation-poll-test",
            agent_id: "a2a-poll-test",
            remote_task_id: Some("remote-poll-task"),
            context_id: Some("remote-poll-context"),
            state: "working",
            interface_snapshot_json: r#"{"binding":"json_rpc"}"#,
            last_event_id: None,
            artifact_snapshot_json: None,
            push_config_json: None,
        })
        .await
        .unwrap();
        let client = Arc::new(MockClient::new(a2a::TaskState::Completed));
        let manager = manager(client.clone(), repo.clone()).await;
        let mut events = manager.subscribe();

        assert!(manager.claim_pending_recovery().await);
        assert!(!manager.claim_pending_recovery().await);
        manager.resume_pending_task().await.unwrap();

        assert!(client.requests.lock().await.is_empty());
        assert_eq!(client.get_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            repo.find_task_by_conversation("conversation-poll-test")
                .await
                .unwrap()
                .unwrap()
                .state,
            "completed"
        );
        let received: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).collect();
        assert!(
            received
                .iter()
                .any(|event| matches!(event, AgentStreamEvent::Finish(_)))
        );
    }
}
