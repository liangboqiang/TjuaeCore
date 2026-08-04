use std::sync::Arc;

use tjuaeui_ai_agent::types::{BuildTaskOptions, SendMessageData};
use tjuaeui_ai_agent::{
    AgentError, AgentInstance, AgentSendError, AgentSessionKind, IWorkerTaskManager, RuntimeAssetFailureReason,
    RuntimeAssetLoadReceipt, RuntimeAssetLoadRequest, verify_runtime_asset_receipt,
};
use tjuaeui_common::{AgentKillReason, AgentType, ConversationStatus, ErrorChain, now_ms};
use tjuaeui_db::models::ConversationRow;
use tokio::sync::oneshot;
use tracing::{debug, error, info, warn};

use crate::agent_health_policy::{AgentHealthAction, AgentHealthPolicy};
use crate::runtime_state::RuntimeLifecycleState;
use crate::runtime_state::TurnClaim;
use crate::service::{
    ConversationService, MAX_SYSTEM_RESPONSE_CONTINUATIONS_PER_TURN, agent_error_top_level_code, persist_session_key,
};
use crate::stream_relay::{RelayOutcome, RelayTerminal, StreamRelay, TurnAttemptSummary};
use crate::trace::{ConversationTraceStartContext, RecordedSpanStatus, RuntimeSpan};
use crate::turn_continuation_policy::{ContinuationDecision, TurnContinuationPolicy};
use crate::turn_recovery_policy::{TurnRecoveryDecision, TurnRecoveryPolicy};
use tjuaeui_api_types::{AgentErrorCode, SendMessageRequest};

fn acp_backend_from_build_options(options: &BuildTaskOptions) -> Option<&str> {
    match &options.context.kind {
        AgentSessionKind::Acp(ctx) => ctx.config.backend.as_deref(),
        AgentSessionKind::A2a(_) => None,
        AgentSessionKind::TjuaeCli(_) => None,
    }
}

fn trace_backend_from_build_options(options: &BuildTaskOptions) -> Option<String> {
    match &options.context.kind {
        AgentSessionKind::Acp(context) => context.config.backend.clone().or_else(|| Some("acp".to_owned())),
        AgentSessionKind::A2a(_) => Some("a2a".to_owned()),
        AgentSessionKind::TjuaeCli(_) => Some("tjuae_cli".to_owned()),
    }
}

pub(crate) struct TurnStartInput {
    pub user_id: String,
    pub conversation: ConversationRow,
    pub request: SendMessageRequest,
    pub required_runtime_mode: Option<String>,
    pub build_options: BuildTaskOptions,
    pub stored_workspace: String,
    pub turn_id: String,
    pub turn_claim: TurnClaim,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConversationTurnStatus {
    Completed,
    Failed,
}

pub(crate) struct ConversationTurnResult {
    pub status: ConversationTurnStatus,
    pub error_message: Option<String>,
}

pub(crate) struct ConversationTurnOrchestrator {
    service: ConversationService,
    task_manager: Arc<dyn IWorkerTaskManager>,
}

struct TurnAttemptInput {
    conv_id: String,
    turn_id: String,
    user_id: String,
    build_options: BuildTaskOptions,
    stored_workspace: String,
    send: SendMessageData,
    msg_id: String,
    allowed_skill_names: Vec<String>,
    required_runtime_mode: Option<String>,
    continuation_count: usize,
    defer_clean_terminal_errors: bool,
}

struct TurnAttemptResult {
    outcome: RelayOutcome,
    summary: TurnAttemptSummary,
    agent_type: AgentType,
    backend: Option<String>,
}

impl ConversationTurnOrchestrator {
    pub fn new(service: ConversationService, task_manager: Arc<dyn IWorkerTaskManager>) -> Self {
        Self { service, task_manager }
    }

    pub fn spawn_user_turn(self, input: TurnStartInput) {
        tokio::spawn(async move {
            let _ = self.run_user_turn(input).await;
        });
    }

    async fn run_attempt(&self, input: TurnAttemptInput) -> Result<TurnAttemptResult, ConversationTurnResult> {
        let availability_agent_id = availability_agent_id(&input.build_options);
        let backend = acp_backend_from_build_options(&input.build_options).map(str::to_owned);
        let runtime_asset_request = input.build_options.runtime_asset_request.clone();
        let build_started_at = now_ms();
        info!(
            conversation_id = %input.conv_id,
            turn_id = %input.turn_id,
            "Agent task build started"
        );

        let agent = match self
            .task_manager
            .get_or_build_task(&input.conv_id, input.build_options)
            .await
        {
            Ok(agent) => agent,
            Err(err) => {
                let top_level_code = agent_error_top_level_code(&err);
                let send_error = AgentSendError::from_agent_error_ref_for_backend(&err, backend.as_deref());
                let top_level_code = if send_error.is_openclaw_gateway_unreachable() {
                    "USER_AGENT_OPENCLAW_GATEWAY_UNREACHABLE"
                } else {
                    top_level_code
                };
                if send_error.is_openclaw_gateway_unreachable() {
                    warn!(
                        conversation_id = %input.conv_id,
                        turn_id = %input.turn_id,
                        backend = "openclaw",
                        error_kind = "openclaw_gateway_unreachable",
                        port = 18789_u16,
                        phase = "turn_build",
                        "OpenClaw Gateway unreachable during ACP startup"
                    );
                }
                error!(
                    conversation_id = %input.conv_id,
                    turn_id = %input.turn_id,
                    error_code = ?send_error.code(),
                    error = %ErrorChain(&err),
                    "Agent task build failed"
                );
                let failure_message = send_error_display_message(&send_error);
                record_agent_session_failure(
                    &self.service,
                    availability_agent_id.as_deref(),
                    "session_build_failed",
                    &failure_message,
                )
                .await;
                self.service
                    .persist_and_broadcast_send_failure_tip(
                        &input.conv_id,
                        &input.turn_id,
                        &send_error,
                        Some(top_level_code),
                    )
                    .await;
                return Err(ConversationTurnResult {
                    status: ConversationTurnStatus::Failed,
                    error_message: Some(failure_message),
                });
            }
        };
        let receipt_started_at = now_ms();
        let runtime_asset_receipt = match confirmed_runtime_asset_receipt(runtime_asset_request.as_ref(), &agent) {
            Ok(receipt) => receipt,
            Err(err) => {
                let _ = self
                    .task_manager
                    .kill(&input.conv_id, Some(AgentKillReason::RuntimeCapabilityChanged));
                let top_level_code = agent_error_top_level_code(&err);
                self.service.record_runtime_span_best_effort(
                    &input.conv_id,
                    &input.turn_id,
                    RuntimeSpan {
                        phase: "receipt",
                        status: RecordedSpanStatus::Failed,
                        started_at: receipt_started_at,
                        ended_at: now_ms(),
                        asset_kind: None,
                        local_asset_id: None,
                        error_code: Some(top_level_code),
                    },
                );
                let failure_message = err.to_string();
                let send_error = AgentSendError::from_agent_error(err);
                error!(
                    conversation_id = %input.conv_id,
                    turn_id = %input.turn_id,
                    error_code = top_level_code,
                    "Agent runtime asset receipt verification failed"
                );
                self.service
                    .persist_and_broadcast_send_failure_tip(
                        &input.conv_id,
                        &input.turn_id,
                        &send_error,
                        Some(top_level_code),
                    )
                    .await;
                return Err(ConversationTurnResult {
                    status: ConversationTurnStatus::Failed,
                    error_message: Some(failure_message),
                });
            }
        };
        if runtime_asset_receipt.is_some() {
            self.service.record_runtime_span_best_effort(
                &input.conv_id,
                &input.turn_id,
                RuntimeSpan {
                    phase: "receipt",
                    status: RecordedSpanStatus::Succeeded,
                    started_at: receipt_started_at,
                    ended_at: now_ms(),
                    asset_kind: None,
                    local_asset_id: None,
                    error_code: None,
                },
            );
        }
        if let Some(receipt) = runtime_asset_receipt
            && let Err(err) = self
                .service
                .record_runtime_asset_snapshot(&input.user_id, &input.conv_id, &input.turn_id, receipt)
                .await
        {
            let _ = self
                .task_manager
                .kill(&input.conv_id, Some(AgentKillReason::RuntimeCapabilityChanged));
            let top_level_code = agent_error_top_level_code(&err);
            let failure_message = err.to_string();
            let send_error = AgentSendError::from_agent_error(err);
            error!(
                conversation_id = %input.conv_id,
                turn_id = %input.turn_id,
                error_code = top_level_code,
                "Agent runtime asset receipt persistence failed"
            );
            self.service
                .persist_and_broadcast_send_failure_tip(
                    &input.conv_id,
                    &input.turn_id,
                    &send_error,
                    Some(top_level_code),
                )
                .await;
            return Err(ConversationTurnResult {
                status: ConversationTurnStatus::Failed,
                error_message: Some(failure_message),
            });
        }

        if let Err(err) = self
            .service
            .maybe_persist_workspace(&input.conv_id, &input.stored_workspace, agent.workspace())
            .await
        {
            let top_level_code = err.error_code();
            let failure_message = err.to_string();
            let send_error = AgentSendError::from_agent_error(err.to_agent_error());
            error!(
                conversation_id = %input.conv_id,
                turn_id = %input.turn_id,
                error_code = err.error_code(),
                error = %ErrorChain(&err),
                "Failed to persist resolved workspace"
            );
            self.service
                .persist_and_broadcast_send_failure_tip(
                    &input.conv_id,
                    &input.turn_id,
                    &send_error,
                    Some(top_level_code),
                )
                .await;
            return Err(ConversationTurnResult {
                status: ConversationTurnStatus::Failed,
                error_message: Some(failure_message),
            });
        }

        info!(
            conversation_id = %input.conv_id,
            turn_id = %input.turn_id,
            agent_type = ?agent.agent_type(),
            elapsed_ms = now_ms().saturating_sub(build_started_at),
            "Agent task ready"
        );

        let persistence = self.service.runtime_persistence();
        let runtime_state = self.service.runtime_state();
        let mut pending_send = Some((input.send, input.msg_id));
        let mut continuation_count = input.continuation_count;
        let continuation_policy = TurnContinuationPolicy::new(MAX_SYSTEM_RESPONSE_CONTINUATIONS_PER_TURN);
        let mut last_outcome = None;
        let mut aggregate_summary = TurnAttemptSummary::default();

        while let Some((current_send, msg_id)) = pending_send.take() {
            let lifecycle = runtime_state.lifecycle_for(&input.conv_id);
            let defer_clean_terminal_errors = input.defer_clean_terminal_errors
                && agent.agent_type() == AgentType::Acp
                && lifecycle == RuntimeLifecycleState::Active
                && aggregate_summary.safe_to_auto_replay();
            let relay = StreamRelay::new(
                input.conv_id.clone(),
                msg_id,
                input.turn_id.clone(),
                input.user_id.clone(),
                self.service.conversation_repo().clone(),
                self.service.broadcaster().clone(),
            )
            .with_skill_resolver(self.service.skill_resolver())
            .with_allowed_skill_names(input.allowed_skill_names.clone())
            .with_runtime_state(Arc::clone(&runtime_state))
            .with_persistence(persistence.clone())
            .with_turn_completion(false)
            .with_defer_clean_terminal_errors(defer_clean_terminal_errors);
            let relay = if let Some(writer) = self.service.trace_writer() {
                relay
                    .with_trace_writer(writer)
                    .with_trace_context(ConversationTraceStartContext {
                        backend: backend.clone(),
                        model: None,
                        mode: input.required_runtime_mode.clone(),
                        input_size: i64::try_from(current_send.content.len()).unwrap_or(i64::MAX),
                    })
            } else {
                relay
            };

            let rx = agent.subscribe();
            if let Some(mode) = input
                .required_runtime_mode
                .as_deref()
                .map(str::trim)
                .filter(|mode| !mode.is_empty())
            {
                match apply_required_runtime_mode(&agent, backend.as_deref(), mode).await {
                    Ok(()) => {
                        info!(
                            conversation_id = %input.conv_id,
                            turn_id = %input.turn_id,
                            mode,
                            "Confirmed required runtime mode before agent turn"
                        );
                    }
                    Err(err) => {
                        let top_level_code = agent_error_top_level_code(&err);
                        let failure_message = err.to_string();
                        let send_error = AgentSendError::from_agent_error(err);
                        error!(
                            conversation_id = %input.conv_id,
                            turn_id = %input.turn_id,
                            mode,
                            error = %failure_message,
                            "Failed to apply required runtime mode before agent turn"
                        );
                        self.service
                            .persist_and_broadcast_send_failure_tip(
                                &input.conv_id,
                                &input.turn_id,
                                &send_error,
                                Some(top_level_code),
                            )
                            .await;
                        return Err(ConversationTurnResult {
                            status: ConversationTurnStatus::Failed,
                            error_message: Some(failure_message),
                        });
                    }
                }
            }
            let send_agent = agent.clone();
            let conv_id_send = input.conv_id.clone();
            let turn_id_for_send = input.turn_id.clone();
            let feedback_service = self.service.clone();
            let feedback_agent_id = availability_agent_id.clone();
            let (send_error_tx, send_error_rx) = oneshot::channel();

            tokio::spawn(async move {
                if let Err(e) = send_agent.send_message(current_send).await {
                    let failure_message = send_error_display_message(&e);
                    record_agent_session_failure(
                        &feedback_service,
                        feedback_agent_id.as_deref(),
                        "session_send_failed",
                        &failure_message,
                    )
                    .await;
                    let task_status = send_agent.status();
                    let agent_type = send_agent.agent_type();
                    error!(
                        conversation_id = %conv_id_send,
                        turn_id = %turn_id_for_send,
                        ?agent_type,
                        ?task_status,
                        error = %ErrorChain(&e),
                        "Agent send_message failed"
                    );
                    if task_status == Some(ConversationStatus::Finished) {
                        debug!(
                            conversation_id = %conv_id_send,
                            turn_id = %turn_id_for_send,
                            ?agent_type,
                            "Agent send_message failed on finished task; relay will prefer any runtime terminal before fallback"
                        );
                    }
                    warn!(
                        conversation_id = %conv_id_send,
                        turn_id = %turn_id_for_send,
                        ?agent_type,
                        code = ?e.code(),
                        ownership = ?e.ownership(),
                        "Agent send_message returned error; offering fallback stream error to relay"
                    );
                    let _ = send_error_tx.send(e);
                }
            });

            let outcome = relay.consume_with_send_error(rx, send_error_rx).await;
            aggregate_summary.merge(&outcome.attempt);

            if let Some(session_key) = agent.get_session_key() {
                persist_session_key(
                    self.service.conversation_repo(),
                    &persistence,
                    &input.conv_id,
                    &session_key,
                )
                .await;
            }

            match continuation_policy.decide(&input.conv_id, continuation_count, &outcome, lifecycle) {
                ContinuationDecision::Continue { content, next_count } => {
                    continuation_count = next_count;
                    let next_turn_msg_id = ConversationService::mint_msg_id();
                    pending_send = Some((
                        SendMessageData {
                            content,
                            msg_id: next_turn_msg_id.clone(),
                            turn_id: Some(input.turn_id.clone()),
                            files: vec![],
                            inject_skills: vec![],
                        },
                        next_turn_msg_id,
                    ));
                }
                ContinuationDecision::Stop(_) => {
                    last_outcome = Some(outcome);
                    break;
                }
            }
        }

        Ok(TurnAttemptResult {
            outcome: last_outcome.unwrap_or_default(),
            summary: aggregate_summary,
            agent_type: agent.agent_type(),
            backend,
        })
    }

    pub(crate) async fn run_user_turn(self, mut input: TurnStartInput) -> ConversationTurnResult {
        let mut turn_claim = input.turn_claim;
        let conv_id = input.conversation.id.clone();
        let turn_id = input.turn_id.clone();
        let runtime_state = self.service.runtime_state();
        self.service
            .start_trace_best_effort(
                &input.user_id,
                &conv_id,
                &turn_id,
                ConversationTraceStartContext {
                    backend: trace_backend_from_build_options(&input.build_options),
                    model: None,
                    mode: input.required_runtime_mode.clone(),
                    input_size: i64::try_from(input.request.content.len()).unwrap_or(i64::MAX),
                },
            )
            .await;
        let allowed_skill_names = input.build_options.context.skills.clone();
        let first_turn_msg_id = ConversationService::mint_msg_id();
        let initial_send = SendMessageData {
            content: input.request.content,
            msg_id: first_turn_msg_id.clone(),
            turn_id: Some(turn_id.clone()),
            files: input.request.files,
            inject_skills: input.request.inject_skills,
        };
        let mut replayed = false;
        let mut replay_started_at = None;
        let mut final_error_message;
        let mut auth_failure = false;
        let mut final_channel_closed = false;

        info!(conversation_id = %conv_id, turn_id = %turn_id, "conversation turn orchestrator started");

        let final_failed = loop {
            let attempt_number = if replayed { 2 } else { 1 };
            let attempt_result = match self
                .run_attempt(TurnAttemptInput {
                    conv_id: conv_id.clone(),
                    turn_id: turn_id.clone(),
                    user_id: input.user_id.clone(),
                    build_options: input.build_options.clone(),
                    stored_workspace: input.stored_workspace.clone(),
                    send: initial_send.clone(),
                    msg_id: first_turn_msg_id.clone(),
                    allowed_skill_names: allowed_skill_names.clone(),
                    required_runtime_mode: input.required_runtime_mode.clone(),
                    continuation_count: 0,
                    defer_clean_terminal_errors: !replayed,
                })
                .await
            {
                Ok(result) => result,
                Err(result) => {
                    final_error_message = result.error_message;
                    break result.status == ConversationTurnStatus::Failed;
                }
            };

            // Track the final attempt's auth signal so the post-loop availability
            // write-back can reflect "needs sign-in" (last iteration wins).
            auth_failure = terminal_is_auth_failure(&attempt_result.outcome);
            final_channel_closed = matches!(attempt_result.outcome.terminal, RelayTerminal::ChannelClosed);

            let lifecycle = runtime_state.lifecycle_for(&conv_id);
            if !attempt_result.outcome.terminal.is_error() {
                final_error_message = None;
                if replayed {
                    info!(
                        conversation_id = %conv_id,
                        turn_id = %turn_id,
                        attempt = attempt_number,
                        elapsed_ms = replay_started_at
                            .map(|started_at| now_ms().saturating_sub(started_at))
                            .unwrap_or_default(),
                        "conversation turn auto replay completed"
                    );
                }
                break false;
            }
            final_error_message = turn_attempt_error_message(&attempt_result.summary);
            if replayed {
                warn!(
                    conversation_id = %conv_id,
                    turn_id = %turn_id,
                    attempt = attempt_number,
                    error_code = ?attempt_result.outcome.terminal.code(),
                    retryable = ?attempt_result.outcome.terminal.retryable(),
                    "conversation turn auto replay failed"
                );
            }

            let mut recovery_outcome = attempt_result.outcome.clone();
            recovery_outcome.attempt = attempt_result.summary.clone();
            let decision = TurnRecoveryPolicy::decide(
                attempt_result.agent_type,
                attempt_result.backend.as_deref(),
                &recovery_outcome,
                lifecycle,
                replayed,
            );

            match decision {
                TurnRecoveryDecision::AutoReplayOnce { reason, .. } => {
                    replay_started_at = Some(now_ms());
                    info!(
                        conversation_id = %conv_id,
                        turn_id = %turn_id,
                        attempt = attempt_number,
                        next_attempt = attempt_number + 1,
                        backend = attempt_result.backend.as_deref().unwrap_or("unknown"),
                        error_code = ?attempt_result.outcome.terminal.code(),
                        retryable = ?attempt_result.outcome.terminal.retryable(),
                        ?reason,
                        "conversation turn auto replay starting"
                    );
                    self.service
                        .evict_acp_task_after_terminal_error(
                            &conv_id,
                            attempt_result.agent_type,
                            &attempt_result.outcome,
                            &self.task_manager,
                        )
                        .await;
                    // ELECTRON-3Q0: attempt 1's dead-anchor self-heal cleared
                    // `acp_session.session_id` mid-turn (persist_side_effects runs
                    // BEFORE the terminal reaches this loop). The turn-start
                    // snapshot still holds the stale anchor — refresh ONLY the
                    // anchor fields so the rebuilt task opens Fresh instead of
                    // re-resuming the same dead session.
                    self.service
                        .refresh_resume_anchor_for_replay(&conv_id, &mut input.build_options)
                        .await;
                    replayed = true;
                    continue;
                }
                TurnRecoveryDecision::None => {
                    if attempt_result.outcome.attempt.terminal_error_deferred
                        && let Some(data) = attempt_result.outcome.attempt.terminal_error.clone()
                    {
                        let send_error = AgentSendError::from_stream_error_data(data);
                        self.service
                            .persist_and_broadcast_send_failure_tip(&conv_id, &turn_id, &send_error, None)
                            .await;
                    }

                    match AgentHealthPolicy::decide(attempt_result.agent_type, &attempt_result.outcome, lifecycle) {
                        AgentHealthAction::Keep => {}
                        AgentHealthAction::EvictAcpTask { .. } => {
                            self.service
                                .evict_acp_task_after_terminal_error(
                                    &conv_id,
                                    attempt_result.agent_type,
                                    &attempt_result.outcome,
                                    &self.task_manager,
                                )
                                .await;
                        }
                    }
                    break true;
                }
            }
        };

        if auth_failure {
            // The agent connected (detection saw it online) but a real turn hit
            // an explicit auth signal — write "needs sign-in" back to its
            // availability so the list stops showing it as plainly usable.
            record_agent_session_failure(
                &self.service,
                availability_agent_id(&input.build_options).as_deref(),
                "auth_required",
                final_error_message
                    .as_deref()
                    .unwrap_or("Agent requires sign-in to run."),
            )
            .await;
        } else if !final_failed {
            record_agent_session_success(&self.service, availability_agent_id(&input.build_options).as_deref()).await;
        }

        let was_cancelled = runtime_state.is_cancelling(&conv_id);
        let (trace_status, trace_incomplete) =
            trace_terminal_outcome(was_cancelled, final_failed, final_channel_closed);
        self.service
            .complete_trace_best_effort(
                &conv_id,
                &turn_id,
                trace_status,
                final_failed.then_some("TURN_FAILED"),
                None,
                trace_incomplete,
            )
            .await;
        let was_deleting = turn_claim.release_for_turn(&turn_id);
        self.service
            .complete_released_turn(&conv_id, &turn_id, was_deleting)
            .await;

        ConversationTurnResult {
            status: if final_failed {
                ConversationTurnStatus::Failed
            } else {
                ConversationTurnStatus::Completed
            },
            error_message: if final_failed { final_error_message } else { None },
        }
    }
}

fn confirmed_runtime_asset_receipt(
    request: Option<&RuntimeAssetLoadRequest>,
    agent: &AgentInstance,
) -> Result<Option<RuntimeAssetLoadReceipt>, AgentError> {
    confirm_runtime_asset_receipt_value(request, agent.runtime_asset_receipt())
}

fn confirm_runtime_asset_receipt_value(
    request: Option<&RuntimeAssetLoadRequest>,
    receipt: Option<RuntimeAssetLoadReceipt>,
) -> Result<Option<RuntimeAssetLoadReceipt>, AgentError> {
    match (request, receipt) {
        (None, None) => Ok(None),
        (Some(_), None) => Err(AgentError::runtime_asset_contract(
            RuntimeAssetFailureReason::ReceiptMissing,
            "运行时未返回实际资产加载回执，已拒绝继续执行",
        )),
        (None, Some(_)) => Err(AgentError::runtime_asset_contract(
            RuntimeAssetFailureReason::ReceiptUnexpected,
            "运行时返回了未请求的资产加载回执，已拒绝继续执行",
        )),
        (Some(request), Some(receipt)) => verify_runtime_asset_receipt(request, receipt)
            .map(Some)
            .map_err(|error| {
                AgentError::runtime_asset_contract(
                    RuntimeAssetFailureReason::ReceiptMismatch,
                    format!("实际资产加载回执与请求不一致：{error}"),
                )
            }),
    }
}

fn availability_agent_id(options: &BuildTaskOptions) -> Option<String> {
    match &options.context.kind {
        AgentSessionKind::Acp(context) => context
            .config
            .agent_id
            .as_deref()
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
        AgentSessionKind::A2a(context) => Some(context.agent_id.clone()),
        AgentSessionKind::TjuaeCli(_) => None,
    }
}

/// True when the turn's terminal is an explicit authentication signal: an
/// `ACP_EMPTY_TURN_NEEDS_AUTH` benign tip (the agent connected but isn't signed
/// in and returned an empty end_turn), or an Error terminal carrying an
/// auth/login error code. Used to reflect "needs sign-in" into the agent's
/// availability even when detection (initialize + session/new, no prompt)
/// showed it online. Non-auth outcomes — generic empty turns, billing,
/// rate-limit, context, network — are deliberately excluded so we don't flip an
/// agent to unavailable for transient or unrelated failures.
fn terminal_is_auth_failure(outcome: &RelayOutcome) -> bool {
    if outcome.attempt.needs_auth {
        return true;
    }
    matches!(
        outcome.terminal.code(),
        Some(
            AgentErrorCode::UserAgentAuthRequired
                | AgentErrorCode::UserLlmProviderAuthFailed
                | AgentErrorCode::UserLlmProviderAwsSsoExpired
        )
    )
}

fn trace_terminal_outcome(was_cancelled: bool, final_failed: bool, final_channel_closed: bool) -> (&'static str, bool) {
    if was_cancelled {
        ("cancelled", true)
    } else if final_channel_closed {
        ("interrupted", true)
    } else if final_failed {
        ("failed", true)
    } else {
        ("succeeded", false)
    }
}

/// codex's canonical full-access mode id (migration 021 / `full_auto_mode_id`,
/// `tjuaeui_common::enums`). What cron's forced full-auto — and a resumed full-access
/// session — persist as `required_runtime_mode`.
const CODEX_CANONICAL_FULL_ACCESS_MODE: &str = "agent-full-access";
/// The LEGACY bare token codex's LIVE catalog actually advertises for the full-access
/// tier (`:danger-full-access`→`full-access`, codex_conn `fill_discovery` /
/// `profile_id_to_legacy_value`).
const CODEX_LEGACY_FULL_ACCESS_MODE: &str = "full-access";

/// ELECTRON-3Q0: align codex's persisted canonical full-access id to whatever the
/// LIVE catalog exposes, mirroring what AcpAgentManager reconcile already does via
/// `normalize_requested_mode_for_available_values`. codex persists the canonical
/// `agent-full-access` (cron forces it for scheduled tasks) but codex's live catalog
/// advertises the legacy bare token `full-access`, so the unnormalized apply hit
/// `set_config_option`'s REJECT ("mode 'agent-full-access' is not one of the available
/// modes") and failed every cron turn before the send.
///
/// Narrow by design (only the required-mode auto-apply path calls this): downgrade
/// ONLY the canonical full-access id, ONLY when the live catalog lacks it but does
/// carry the legacy token. Every other case returns the value unchanged — a live
/// catalog that carries the canonical id keeps it, and a catalog with NO full-access
/// tier at all leaves the value so `set_config_option` still REJECTs (never silently
/// pick a weaker mode). Non-codex backends and non-full-access modes pass straight
/// through. An empty catalog is left untouched too (set_config_option is permissive
/// on an empty/not-yet-discovered catalog).
fn normalize_required_mode_for_catalog(backend: Option<&str>, mode: &str, available_ids: &[String]) -> String {
    if backend != Some("codex") || mode != CODEX_CANONICAL_FULL_ACCESS_MODE {
        return mode.to_owned();
    }
    let has_canonical = available_ids.iter().any(|id| id == CODEX_CANONICAL_FULL_ACCESS_MODE);
    let has_legacy = available_ids.iter().any(|id| id == CODEX_LEGACY_FULL_ACCESS_MODE);
    if !has_canonical && has_legacy {
        return CODEX_LEGACY_FULL_ACCESS_MODE.to_owned();
    }
    mode.to_owned()
}

/// Read the live mode-catalog ids for normalization. Only consulted when a codex
/// canonical full-access id is being applied (see `resolve_required_runtime_mode`);
/// a catalog read failure returns None so the caller falls back to the requested mode
/// unnormalized (= pre-fix behavior).
async fn live_mode_catalog_ids(agent: &AgentInstance) -> Option<Vec<String>> {
    match agent.get_config_options().await {
        Ok(resp) => Some(
            resp.config_options
                .into_iter()
                .find(|opt| opt.id == "mode")
                .map(|opt| opt.options.into_iter().map(|o| o.value).collect())
                .unwrap_or_default(),
        ),
        Err(err) => {
            warn!(
                error = %err,
                "apply mode: live catalog read failed — applying the requested mode unnormalized"
            );
            None
        }
    }
}

/// Resolve the mode to actually apply. For codex's canonical full-access id, align it
/// to the live catalog (ELECTRON-3Q0); every other backend/mode is returned verbatim
/// without touching the catalog.
async fn resolve_required_runtime_mode(agent: &AgentInstance, backend: Option<&str>, mode: &str) -> String {
    if backend != Some("codex") || mode != CODEX_CANONICAL_FULL_ACCESS_MODE {
        return mode.to_owned();
    }
    let Some(available_ids) = live_mode_catalog_ids(agent).await else {
        return mode.to_owned();
    };
    let effective = normalize_required_mode_for_catalog(backend, mode, &available_ids);
    if effective != mode {
        info!(
            requested = mode,
            effective = %effective,
            "apply mode: aligned codex canonical full-access to the live catalog (ELECTRON-3Q0)"
        );
    }
    effective
}

async fn apply_required_runtime_mode(
    agent: &AgentInstance,
    backend: Option<&str>,
    mode: &str,
) -> Result<(), AgentError> {
    let effective = resolve_required_runtime_mode(agent, backend, mode).await;
    agent.set_config_option("mode", &effective).await?;
    Ok(())
}

fn send_error_display_message(error: &AgentSendError) -> String {
    error
        .stream_error()
        .detail
        .clone()
        .unwrap_or_else(|| error.stream_error().message.clone())
}

fn turn_attempt_error_message(summary: &TurnAttemptSummary) -> Option<String> {
    summary.terminal_error.as_ref().map(|error| {
        error
            .detail
            .as_deref()
            .filter(|detail| !detail.trim().is_empty())
            .unwrap_or(error.message.as_str())
            .to_owned()
    })
}

async fn record_agent_session_failure(
    service: &ConversationService,
    agent_id: Option<&str>,
    code: &str,
    message: &str,
) {
    let Some(agent_id) = agent_id else {
        return;
    };
    let Some(feedback) = service.agent_availability_feedback() else {
        return;
    };
    if let Err(error) = feedback.record_session_failure(agent_id, code, message).await {
        warn!(
            agent_id,
            code,
            error = %ErrorChain(&error),
            "Failed to record agent availability session failure"
        );
    }
}

async fn record_agent_session_success(service: &ConversationService, agent_id: Option<&str>) {
    let Some(agent_id) = agent_id else {
        return;
    };
    let Some(feedback) = service.agent_availability_feedback() else {
        return;
    };
    if let Err(error) = feedback.record_session_success(agent_id).await {
        warn!(
            agent_id,
            error = %ErrorChain(&error),
            "Failed to record agent availability session success"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream_relay::RelayTerminal;

    fn runtime_asset_request() -> RuntimeAssetLoadRequest {
        RuntimeAssetLoadRequest::new(
            vec![tjuaeui_ai_agent::RuntimeAssetRef {
                local_asset_id: "assistant-a".into(),
                kind: "assistant".into(),
                local_definition_digest: format!("sha256-{}", "a".repeat(64)),
                runtime_content_digest: format!("sha256-{}", "b".repeat(64)),
                upstream_package: None,
                upstream_asset_id: None,
                upstream_version: None,
                upstream_revision: None,
            }],
            Vec::new(),
        )
        .unwrap()
        .unwrap()
    }

    fn assert_runtime_asset_failure_reason(error: AgentError, expected: RuntimeAssetFailureReason) {
        assert!(matches!(
            error,
            AgentError::RuntimeAssetContract { reason, .. } if reason == expected
        ));
    }

    #[test]
    fn runtime_asset_receipt_boundary_is_fail_closed_with_stable_reasons() {
        let request = runtime_asset_request();
        let valid_receipt = tjuaeui_ai_agent::core_only_runtime_asset_receipt(&request).unwrap();

        assert_runtime_asset_failure_reason(
            confirm_runtime_asset_receipt_value(Some(&request), None).unwrap_err(),
            RuntimeAssetFailureReason::ReceiptMissing,
        );
        assert_runtime_asset_failure_reason(
            confirm_runtime_asset_receipt_value(None, Some(valid_receipt.clone())).unwrap_err(),
            RuntimeAssetFailureReason::ReceiptUnexpected,
        );

        let mut mismatched_receipt = valid_receipt.clone();
        mismatched_receipt.assets[0].runtime_content_digest = format!("sha256-{}", "c".repeat(64));
        assert_runtime_asset_failure_reason(
            confirm_runtime_asset_receipt_value(Some(&request), Some(mismatched_receipt)).unwrap_err(),
            RuntimeAssetFailureReason::ReceiptMismatch,
        );

        assert_eq!(
            confirm_runtime_asset_receipt_value(Some(&request), Some(valid_receipt.clone())).unwrap(),
            Some(valid_receipt)
        );
        assert_eq!(confirm_runtime_asset_receipt_value(None, None).unwrap(), None);
    }

    fn finish_outcome(needs_auth: bool) -> RelayOutcome {
        RelayOutcome {
            system_responses: vec![],
            terminal: RelayTerminal::Finish,
            attempt: TurnAttemptSummary {
                needs_auth,
                ..Default::default()
            },
        }
    }

    fn error_outcome(code: AgentErrorCode) -> RelayOutcome {
        RelayOutcome {
            system_responses: vec![],
            terminal: RelayTerminal::Error {
                code: Some(code),
                retryable: None,
            },
            attempt: TurnAttemptSummary::default(),
        }
    }

    // ELECTRON-3Q0: cron forces codex's canonical full-access id (`agent-full-access`,
    // `full_auto_mode_id`) but codex's LIVE catalog advertises the legacy bare token
    // (`full-access`). The direct-CLI turn-time apply must align the two, mirroring
    // AcpAgentManager reconcile — otherwise `set_config_option` REJECTs
    // ("mode 'agent-full-access' is not one of the available modes") and every cron
    // turn fails before the send.
    #[test]
    fn codex_canonical_full_access_downgrades_to_legacy_when_catalog_lacks_canonical() {
        let ids = vec!["auto".to_string(), "full-access".to_string(), "read-only".to_string()];
        assert_eq!(
            normalize_required_mode_for_catalog(Some("codex"), "agent-full-access", &ids),
            "full-access",
            "canonical id absent but legacy present → downgrade so set_config_option accepts"
        );
    }

    #[test]
    fn codex_canonical_full_access_kept_when_catalog_has_it() {
        let ids = vec!["auto".to_string(), "agent-full-access".to_string()];
        assert_eq!(
            normalize_required_mode_for_catalog(Some("codex"), "agent-full-access", &ids),
            "agent-full-access",
            "a live catalog that carries the canonical id keeps it verbatim"
        );
    }

    #[test]
    fn codex_full_access_left_for_local_reject_when_no_full_access_tier() {
        // No full-access tier at all → leave the value so set_config_option still
        // REJECTs; never silently pick a weaker mode.
        let ids = vec!["auto".to_string(), "read-only".to_string()];
        assert_eq!(
            normalize_required_mode_for_catalog(Some("codex"), "agent-full-access", &ids),
            "agent-full-access"
        );
    }

    #[test]
    fn non_full_access_required_mode_passes_through() {
        let ids = vec!["auto".to_string(), "full-access".to_string()];
        assert_eq!(
            normalize_required_mode_for_catalog(Some("codex"), "auto", &ids),
            "auto",
            "only the canonical full-access id is normalized; other required modes pass through"
        );
    }

    #[test]
    fn non_codex_backend_never_normalized() {
        let ids = vec!["full-access".to_string()];
        assert_eq!(
            normalize_required_mode_for_catalog(Some("claude"), "agent-full-access", &ids),
            "agent-full-access",
            "the codex canonical/legacy mapping must not touch other backends"
        );
    }

    #[test]
    fn empty_catalog_leaves_mode_unchanged() {
        // An empty / not-yet-discovered catalog is permissive at set_config_option,
        // so leave the requested value untouched here.
        assert_eq!(
            normalize_required_mode_for_catalog(Some("codex"), "agent-full-access", &[]),
            "agent-full-access"
        );
    }

    #[test]
    fn needs_auth_empty_turn_is_auth_failure() {
        assert!(terminal_is_auth_failure(&finish_outcome(true)));
    }

    #[test]
    fn plain_finish_is_not_auth_failure() {
        // A generic empty turn (or any normal finish) must NOT flip availability.
        assert!(!terminal_is_auth_failure(&finish_outcome(false)));
    }

    #[test]
    fn ordinary_channel_close_is_an_incomplete_interrupted_trace() {
        assert_eq!(trace_terminal_outcome(false, false, true), ("interrupted", true));
        assert_eq!(trace_terminal_outcome(false, false, false), ("succeeded", false));
    }

    #[test]
    fn explicit_auth_error_codes_are_auth_failure() {
        assert!(terminal_is_auth_failure(&error_outcome(
            AgentErrorCode::UserAgentAuthRequired
        )));
        assert!(terminal_is_auth_failure(&error_outcome(
            AgentErrorCode::UserLlmProviderAuthFailed
        )));
        assert!(terminal_is_auth_failure(&error_outcome(
            AgentErrorCode::UserLlmProviderAwsSsoExpired
        )));
    }

    #[test]
    fn non_auth_errors_are_not_auth_failure() {
        assert!(!terminal_is_auth_failure(&error_outcome(
            AgentErrorCode::UnknownUpstreamError
        )));
        assert!(!terminal_is_auth_failure(&error_outcome(
            AgentErrorCode::UserLlmProviderRateLimited
        )));
        assert!(!terminal_is_auth_failure(&error_outcome(
            AgentErrorCode::UserLlmProviderBillingRequired
        )));
    }
}
