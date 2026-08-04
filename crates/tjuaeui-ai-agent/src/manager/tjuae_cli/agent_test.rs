use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use tjuae_config::config::{McpServerConfig, TransportType};
use tokio::sync::broadcast::error::TryRecvError;
use tokio::time::timeout;

use super::*;
use crate::agent_task::IAgentTask;
use crate::protocol::events::FinishEventData;

fn runtime_asset(id: &str, kind: &str, digest: char) -> crate::runtime_assets::RuntimeAssetRef {
    crate::runtime_assets::RuntimeAssetRef {
        local_asset_id: id.into(),
        kind: kind.into(),
        local_definition_digest: format!("sha256-{}", digest.to_string().repeat(64)),
        runtime_content_digest: format!("sha256-{}", digest.to_string().repeat(64)),
        upstream_package: None,
        upstream_asset_id: None,
        upstream_version: None,
        upstream_revision: None,
    }
}

#[test]
fn managed_skill_receipt_combines_cli_attestation_with_core_assistant() {
    let assistant = runtime_asset("assistant-a", "assistant", 'a');
    let skill = runtime_asset("skill-a", "skill", 'b');
    let request = RuntimeAssetLoadRequest::new(
        vec![assistant.clone()],
        vec![crate::runtime_assets::RuntimeManagedSkillRef {
            asset: skill.clone(),
            root: PathBuf::from("redacted"),
        }],
    )
    .unwrap()
    .unwrap();
    let receipt = CliRuntimeAssetSnapshot {
        runtime_snapshot_id: request.runtime_snapshot_id.clone(),
        assets: vec![cli_runtime_asset_ref(&skill)],
    };

    let receipt = verified_runtime_asset_receipt(Some(&request), Some(receipt))
        .unwrap()
        .unwrap();
    assert_eq!(receipt.runtime_snapshot_id, request.runtime_snapshot_id);
    assert_eq!(receipt.assets, vec![assistant, skill]);
}

#[test]
fn four_kind_receipt_combines_core_and_cli_attestations() {
    let assistant = runtime_asset("assistant-a", "assistant", 'a');
    let engine = runtime_asset("engine-a", "engineAdapter", 'b');
    let skill = runtime_asset("skill-a", "skill", 'c');
    let mcp = runtime_asset("mcp-a", "mcp", 'd');
    let request = RuntimeAssetLoadRequest::new_with_runtime_assets(
        vec![assistant.clone()],
        vec![engine.clone()],
        vec![crate::runtime_assets::RuntimeManagedSkillRef {
            asset: skill.clone(),
            root: PathBuf::from("redacted"),
        }],
        vec![crate::runtime_assets::RuntimeManagedMcpRef {
            asset: mcp.clone(),
            server_name: "docs".into(),
        }],
    )
    .unwrap()
    .unwrap();
    let receipt = CliRuntimeAssetSnapshot {
        runtime_snapshot_id: request.runtime_snapshot_id.clone(),
        assets: vec![
            cli_runtime_asset_ref(&engine),
            cli_runtime_asset_ref(&skill),
            cli_runtime_asset_ref(&mcp),
        ],
    };

    let receipt = verified_runtime_asset_receipt(Some(&request), Some(receipt))
        .unwrap()
        .unwrap();

    assert_eq!(receipt.assets, vec![assistant, engine, mcp, skill]);
}

#[test]
fn managed_skill_receipt_fails_closed_when_cli_attests_different_content() {
    let skill = runtime_asset("skill-a", "skill", 'b');
    let request = RuntimeAssetLoadRequest::new(
        Vec::new(),
        vec![crate::runtime_assets::RuntimeManagedSkillRef {
            asset: skill.clone(),
            root: PathBuf::from("redacted"),
        }],
    )
    .unwrap()
    .unwrap();
    let receipt = CliRuntimeAssetSnapshot {
        runtime_snapshot_id: request.runtime_snapshot_id.clone(),
        assets: vec![cli_runtime_asset_ref(&runtime_asset("skill-a", "skill", 'c'))],
    };

    assert!(matches!(
        verified_runtime_asset_receipt(Some(&request), Some(receipt)),
        Err(AgentError::RuntimeAssetContract {
            reason: RuntimeAssetFailureReason::ReceiptMismatch,
            ..
        })
    ));
}

#[test]
fn managed_skill_receipt_fails_closed_when_cli_snapshot_is_missing() {
    let request = RuntimeAssetLoadRequest::new(
        Vec::new(),
        vec![crate::runtime_assets::RuntimeManagedSkillRef {
            asset: runtime_asset("skill-a", "skill", 'b'),
            root: PathBuf::from("redacted"),
        }],
    )
    .unwrap()
    .unwrap();

    assert!(matches!(
        verified_runtime_asset_receipt(Some(&request), None),
        Err(AgentError::RuntimeAssetContract {
            reason: RuntimeAssetFailureReason::ReceiptMissing,
            ..
        })
    ));
}

#[test]
fn cli_snapshot_fails_closed_when_no_runtime_assets_were_requested() {
    let snapshot = CliRuntimeAssetSnapshot {
        runtime_snapshot_id: "sha256-unrequested".into(),
        assets: Vec::new(),
    };

    assert!(matches!(
        verified_runtime_asset_receipt(None, Some(snapshot)),
        Err(AgentError::RuntimeAssetContract {
            reason: RuntimeAssetFailureReason::ReceiptUnexpected,
            ..
        })
    ));
}

#[test]
fn shared_v2_fixture_is_accepted_by_core_and_cli_contracts() {
    let fixture = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/runtime-asset-snapshot.v2.json"
    ));
    let snapshot: CliRuntimeAssetSnapshot = serde_json::from_str(fixture).expect("v2 fixture should deserialize");
    let requested_assets = snapshot
        .assets
        .iter()
        .cloned()
        .map(runtime_asset_ref_from_cli)
        .collect::<Vec<_>>();
    let assistant = requested_assets
        .iter()
        .find(|asset| asset.kind == "assistant")
        .unwrap()
        .clone();
    let engine = requested_assets
        .iter()
        .find(|asset| asset.kind == "engineAdapter")
        .unwrap()
        .clone();
    let skill = requested_assets
        .iter()
        .find(|asset| asset.kind == "skill")
        .unwrap()
        .clone();
    let mcp = requested_assets
        .iter()
        .find(|asset| asset.kind == "mcp")
        .unwrap()
        .clone();
    let request = RuntimeAssetLoadRequest::new_with_runtime_assets(
        vec![assistant],
        vec![engine],
        vec![crate::runtime_assets::RuntimeManagedSkillRef {
            asset: skill,
            root: PathBuf::from("redacted"),
        }],
        vec![crate::runtime_assets::RuntimeManagedMcpRef {
            asset: mcp,
            server_name: "docs".into(),
        }],
    )
    .unwrap()
    .unwrap();

    assert_eq!(request.runtime_snapshot_id, snapshot.runtime_snapshot_id);
    let mut cli_snapshot = snapshot;
    cli_snapshot.assets.retain(|asset| asset.kind != "assistant");
    let receipt = verified_runtime_asset_receipt(Some(&request), Some(cli_snapshot))
        .unwrap()
        .unwrap();
    assert_eq!(receipt.runtime_snapshot_id, request.runtime_snapshot_id);
    assert_eq!(receipt.assets, requested_assets);
}

async fn assert_no_stop_signal(agent: &TjuaeCliAgentManager) {
    let notified = agent.cancel_notify.notified();
    tokio::pin!(notified);

    assert!(
        timeout(Duration::from_millis(20), &mut notified).await.is_err(),
        "idle stop must not leave a stale cancellation signal for the next turn"
    );
}

fn make_test_config() -> TjuaeCliResolvedConfig {
    TjuaeCliResolvedConfig {
        provider: "anthropic".into(),
        api_key: "sk-test-key".into(),
        model: "claude-sonnet-4-20250514".into(),
        base_url: None,
        system_prompt: None,
        max_tokens: None,
        max_turns: None,
        max_tool_call_malformed_turns: None,
        max_tool_call_failure_turns: None,
        compat_overrides: Default::default(),
        session_directory: env::temp_dir().join("tjuaecli-test-sessions"),
        session_mode: None,
        skills: Vec::new(),
        extra_mcp_servers: HashMap::new(),
        bedrock_config: None,
        runtime_env: Vec::new(),
        prompt_dump_dir: None,
    }
}

fn make_cli_args(project_dir: PathBuf, provider: &str, model: &str) -> CliArgs {
    CliArgs {
        provider: Some(provider.to_owned()),
        api_key: Some("sk-test-key".to_owned()),
        base_url: None,
        model: Some(model.to_owned()),
        max_tokens: None,
        thinking: None,
        thinking_budget: None,
        max_turns: None,
        max_tool_call_malformed_turns: None,
        max_tool_call_failure_turns: None,
        system_prompt: None,
        profile: None,
        auto_approve: false,
        project_dir: Some(project_dir),
    }
}

#[test]
fn resolve_tjuaeui_config_discards_standalone_max_token_settings() {
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join(".tjuae.toml"),
        r#"
[default]
max_tokens = 1234

[providers.openai.compat]
default_max_tokens = 2345

[[providers.openai.compat.model_max_tokens]]
pattern = "gpt-test"
max_tokens = 3456
"#,
    )
    .unwrap();
    let cli_args = make_cli_args(project.path().to_path_buf(), "openai", "gpt-test");

    let standalone = Config::resolve(&cli_args).unwrap();
    assert_eq!(standalone.max_tokens, Some(1234));
    assert_eq!(standalone.compat.default_max_tokens_for_model("gpt-test"), Some(3456));

    let embedded = resolve_tjuaeui_config(&cli_args).unwrap();
    assert_eq!(embedded.max_tokens, None);
    assert_eq!(embedded.compat.default_max_tokens_for_model("gpt-test"), None);
}

#[test]
fn resolve_tjuaeui_config_keeps_builtin_provider_max_token_policy() {
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join(".tjuae.toml"),
        r#"
[providers.anthropic.compat]
default_max_tokens = 42

[[providers.anthropic.compat.model_max_tokens]]
pattern = "claude-sonnet-4-6"
max_tokens = 42
"#,
    )
    .unwrap();
    let cli_args = make_cli_args(project.path().to_path_buf(), "anthropic", "claude-sonnet-4-6");

    let embedded = resolve_tjuaeui_config(&cli_args).unwrap();
    assert_eq!(embedded.max_tokens, None);
    assert_eq!(
        embedded.compat.default_max_tokens_for_model("claude-sonnet-4-6"),
        Some(128_000)
    );
}

#[test]
fn tjuae_cli_final_input_dump_value_contains_raw_split_input_and_context() {
    let mut mcp_env = HashMap::new();
    mcp_env.insert("TOKEN".to_owned(), "raw-token-value".to_owned());

    let mut mcp_servers = HashMap::new();
    mcp_servers.insert(
        "raw-mcp".to_owned(),
        McpServerConfig {
            transport: TransportType::Stdio,
            command: Some("/bin/raw-mcp".to_owned()),
            args: Some(vec!["--serve".to_owned()]),
            env: Some(mcp_env),
            url: None,
            headers: None,
            deferred: Some(false),
            startup_timeout_ms: None,
        },
    );

    let context = TjuaeCliFinalInputDumpContext {
        dump_dir: PathBuf::from("/tmp/prompt-dumps"),
        provider: "openai".to_owned(),
        model: "gpt-test".to_owned(),
        base_url: Some("https://example.test/v1".to_owned()),
        system_prompt: Some("assistant rule raw".to_owned()),
        session_mode: Some("yolo".to_owned()),
        skills: vec!["tjuaeui-config".to_owned()],
        mcp_servers,
        runtime_env: vec![("TJUAE_RAW".to_owned(), "raw-env-value".to_owned())],
    };
    let data = SendMessageData {
        content: "team wake raw content".to_owned(),
        msg_id: "msg-tjuaecli-final".to_owned(),
        turn_id: Some("turn-tjuaecli-final".to_owned()),
        files: Vec::new(),
        inject_skills: Vec::new(),
    };

    let value = build_tjuae_cli_final_input_dump_value("conv-tjuaecli", "/workspace", &context, &data);

    assert_eq!(value["kind"], "tjuaecli-final-input");
    assert_eq!(value["backend"], "tjuaecli");
    assert_eq!(value["conversation_id"], "conv-tjuaecli");
    assert_eq!(value["msg_id"], "msg-tjuaecli-final");
    assert_eq!(value["turn_id"], "turn-tjuaecli-final");
    assert_eq!(value["input"]["system_prompt"], "assistant rule raw");
    assert_eq!(value["input"]["user_content"], "team wake raw content");
    assert_eq!(value["resolved_context"]["provider"], "openai");
    assert_eq!(value["resolved_context"]["model"], "gpt-test");
    assert_eq!(value["resolved_context"]["workspace"]["path"], "/workspace");
    assert_eq!(value["resolved_context"]["skills"][0], "tjuaeui-config");
    assert_eq!(
        value["resolved_context"]["mcp_servers"]["raw-mcp"]["env"]["TOKEN"],
        "raw-token-value"
    );
    assert_eq!(value["resolved_context"]["runtime_env"][0][1], "raw-env-value");
}

#[tokio::test]
async fn tjuae_cli_agent_returns_correct_type() {
    let agent = TjuaeCliAgentManager::new("conv-1".into(), "/project".into(), make_test_config(), None)
        .await
        .unwrap();
    assert_eq!(agent.agent_type(), AgentType::TjuaeCli);
    assert_eq!(agent.workspace(), "/project");
    assert_eq!(agent.conversation_id(), "conv-1");
}

#[tokio::test]
async fn tjuae_cli_agent_initial_status_is_pending() {
    let agent = TjuaeCliAgentManager::new("conv-1".into(), "/project".into(), make_test_config(), None)
        .await
        .unwrap();
    assert_eq!(agent.status(), Some(ConversationStatus::Pending));
}

#[tokio::test]
async fn tjuae_cli_agent_subscribe_returns_receiver() {
    let agent = TjuaeCliAgentManager::new("conv-1".into(), "/project".into(), make_test_config(), None)
        .await
        .unwrap();
    let _rx = agent.subscribe();
}

#[tokio::test]
async fn tjuae_cli_agent_kill_succeeds() {
    let agent = TjuaeCliAgentManager::new("conv-1".into(), "/project".into(), make_test_config(), None)
        .await
        .unwrap();
    assert!(agent.kill(None).is_ok());
    // Idle kill only clears transient state; task-manager removal owns lifecycle cleanup.
    assert_eq!(agent.status(), Some(ConversationStatus::Pending));
}

#[tokio::test]
async fn tjuae_cli_agent_kill_with_reason_succeeds() {
    let agent = TjuaeCliAgentManager::new("conv-1".into(), "/project".into(), make_test_config(), None)
        .await
        .unwrap();
    assert!(agent.kill(Some(AgentKillReason::IdleTimeout)).is_ok());
}

#[tokio::test]
async fn tjuae_cli_agent_kill_running_turn_sends_stop_signal() {
    let agent = TjuaeCliAgentManager::new("conv-1".into(), "/project".into(), make_test_config(), None)
        .await
        .unwrap();
    agent.runtime.reset_for_new_turn(ConversationStatus::Running);

    let notified = agent.cancel_notify.notified();
    tokio::pin!(notified);
    assert!(timeout(Duration::from_millis(20), &mut notified).await.is_err());

    agent
        .kill(Some(AgentKillReason::ConversationDeleted))
        .expect("kill should request stop");

    timeout(Duration::from_millis(50), &mut notified)
        .await
        .expect("running kill should wake in-flight turn");
}

#[tokio::test]
async fn tjuae_cli_agent_kill_and_wait_waits_for_running_turn_terminal() {
    let agent = TjuaeCliAgentManager::new("conv-1".into(), "/project".into(), make_test_config(), None)
        .await
        .unwrap();
    agent.runtime.reset_for_new_turn(ConversationStatus::Running);

    let wait = agent.kill_and_wait(Some(AgentKillReason::ConversationDeleted));
    tokio::pin!(wait);
    assert!(
        timeout(Duration::from_millis(20), &mut wait).await.is_err(),
        "kill_and_wait must not return before a running turn reaches a terminal event"
    );

    agent.runtime.emit_finish(None);
    agent.turn_finished_notify.notify_waiters();

    timeout(Duration::from_millis(50), &mut wait)
        .await
        .expect("kill_and_wait should return after terminal notification");
}

#[tokio::test]
async fn tjuae_cli_agent_kill_idle_turn_does_not_leave_stale_stop_signal() {
    let agent = TjuaeCliAgentManager::new("conv-1".into(), "/project".into(), make_test_config(), None)
        .await
        .unwrap();

    agent
        .kill(Some(AgentKillReason::ConversationDeleted))
        .expect("idle kill should be harmless");

    assert_no_stop_signal(&agent).await;
}

#[tokio::test]
async fn tjuae_cli_agent_confirmations_initially_empty() {
    let agent = TjuaeCliAgentManager::new("conv-1".into(), "/project".into(), make_test_config(), None)
        .await
        .unwrap();
    assert!(agent.get_confirmations().is_empty());
}

#[tokio::test]
async fn tjuae_cli_agent_get_slash_commands_does_not_wait_for_engine_lock() {
    let agent = TjuaeCliAgentManager::new("conv-1".into(), "/project".into(), make_test_config(), None)
        .await
        .unwrap();

    let _engine_guard = agent.engine.lock().await;
    let commands = timeout(Duration::from_millis(50), agent.get_slash_commands())
        .await
        .expect("slash command metadata should not wait for an active engine run")
        .unwrap();

    assert!(!commands.is_empty());
}

#[tokio::test]
async fn tjuae_cli_agent_check_approval_returns_false_by_default() {
    let agent = TjuaeCliAgentManager::new("conv-1".into(), "/project".into(), make_test_config(), None)
        .await
        .unwrap();
    assert!(!agent.check_approval("any_action", None));
}

#[tokio::test]
async fn stop_only_signals_in_flight_run() {
    let agent = TjuaeCliAgentManager::new("conv-stop".into(), "/project".into(), make_test_config(), None)
        .await
        .unwrap();
    let mut rx = agent.subscribe();

    agent.cancel().await.unwrap();

    assert_eq!(agent.status(), Some(ConversationStatus::Pending));
    assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
    assert_no_stop_signal(&agent).await;
}

#[tokio::test]
async fn runtime_can_emit_error_and_finish() {
    let agent = TjuaeCliAgentManager::new("conv-err".into(), "/project".into(), make_test_config(), None)
        .await
        .unwrap();
    let mut rx = agent.subscribe();

    agent.runtime.emit_error("test error");
    // emit_error sets status to Finished, so emit_finish is a no-op here.
    // We emit directly for the Finish broadcast path test:
    agent
        .runtime
        .emit(AgentStreamEvent::Finish(FinishEventData { session_id: None }));

    match rx.try_recv().unwrap() {
        AgentStreamEvent::Error(data) => assert_eq!(data.message, "test error"),
        other => panic!("Expected Error, got {:?}", other),
    }
    match rx.try_recv().unwrap() {
        AgentStreamEvent::Finish(_) => {}
        other => panic!("Expected Finish, got {:?}", other),
    }
}
