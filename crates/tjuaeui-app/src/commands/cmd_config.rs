//! `tjuaecore config` subcommand: agent-facing automation CLI for TjuaeUI config.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::process::ExitCode;

use reqwest::Method;
use serde_json::{Map, Value, json};

use crate::cli::{
    ConfigAgentCustomCommand, ConfigAgentOverridesCommand, ConfigAgentsArgs, ConfigAgentsCommand, ConfigArgs,
    ConfigAssistantFileCommand, ConfigAssistantsArgs, ConfigAssistantsCommand, ConfigCommand, ConfigConversationArgs,
    ConfigConversationCommand, ConfigCronArgs, ConfigCronCommand, ConfigCronCurrentArgs, ConfigCronCurrentCommand,
    ConfigCronJobSkillCommand, ConfigCronJobsArgs, ConfigCronJobsCommand, ConfigMcpArgs, ConfigMcpCommand,
    ConfigMcpOauthCommand, ConfigMcpServersCommand, ConfigProviderModelsCommand, ConfigProvidersArgs,
    ConfigProvidersCommand, ConfigSettingsArgs, ConfigSettingsClientCommand, ConfigSettingsCommand, ConfigSkillsArgs,
    ConfigSkillsCommand,
};
use crate::commands::config_capabilities;

const ENV_BASE_URL: &str = "TJUAE_BASE_URL";
const ENV_CONVERSATION_ID: &str = "TJUAE_CONVERSATION_ID";
const ENV_USER_ID: &str = "TJUAE_USER_ID";

pub async fn run_config(args: ConfigArgs) -> ExitCode {
    match run(args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            error.log_failure();
            eprintln!("{}", error.stderr_line());
            error.exit_code()
        }
    }
}

async fn run(args: ConfigArgs) -> Result<(), ConfigError> {
    let client = reqwest::Client::new();
    match args.command {
        ConfigCommand::Capabilities => print_envelope(config_capabilities::data(), meta(None), "config capabilities"),
        ConfigCommand::Context => run_context(&client).await,
        ConfigCommand::Conversation(args) => run_conversation(&client, args).await,
        ConfigCommand::Assistants(args) => run_assistants(&client, args).await,
        ConfigCommand::Skills(args) => run_skills(&client, args).await,
        ConfigCommand::Mcp(args) => run_mcp(&client, args).await,
        ConfigCommand::Providers(args) => run_providers(&client, args).await,
        ConfigCommand::Settings(args) => run_settings(&client, args).await,
        ConfigCommand::Agents(args) => run_agents(&client, args).await,
        ConfigCommand::Cron(args) => run_cron(&client, args).await,
    }
}

async fn run_context(client: &reqwest::Client) -> Result<(), ConfigError> {
    let command = "config context";
    let env = ConfigEnv::from_env(command)?;
    let conversation = fetch_current_conversation(client, &env, command).await?;
    let assistant = conversation.get("assistant").cloned().unwrap_or(Value::Null);
    print_envelope(
        json!({
            "user_id": env.user_id,
            "conversation_id": env.conversation_id,
            "assistant": assistant,
            "base_url": env.base_url,
        }),
        meta(None),
        command,
    )
}

async fn run_conversation(client: &reqwest::Client, args: ConfigConversationArgs) -> Result<(), ConfigError> {
    match args.command {
        ConfigConversationCommand::Rename => {
            run_id_payload_request_with_resource_readback(
                client,
                "config conversation rename",
                Method::PATCH,
                IdRoute::new("/api/conversations", "", "conversation_id"),
                IdRoute::new("/api/conversations", "", "conversation_id"),
                false,
            )
            .await
        }
    }
}

async fn run_assistants(client: &reqwest::Client, args: ConfigAssistantsArgs) -> Result<(), ConfigError> {
    match args.command {
        ConfigAssistantsCommand::List => run_assistant_list(client).await,
        ConfigAssistantsCommand::Get => run_assistant_get(client).await,
        ConfigAssistantsCommand::Create => run_assistant_create(client).await,
        ConfigAssistantsCommand::Settings => run_assistant_settings(client).await,
        ConfigAssistantsCommand::Delete => run_assistant_delete(client).await,
        ConfigAssistantsCommand::Copy => run_assistant_copy(client).await,
        ConfigAssistantsCommand::Preferences => run_assistant_preferences(client).await,
        ConfigAssistantsCommand::File(args) => run_assistant_file(client, args.command).await,
        ConfigAssistantsCommand::Prepare => run_assistant_prepare(client).await,
        ConfigAssistantsCommand::Activate => run_assistant_activate(client).await,
        ConfigAssistantsCommand::Export => run_assistant_export(client).await,
        ConfigAssistantsCommand::Publish => run_assistant_publish(client).await,
    }
}

async fn run_assistant_list(client: &reqwest::Client) -> Result<(), ConfigError> {
    let command = "config assistants list";
    let env = ConfigEnv::from_env(command)?;
    let mine = request_json(
        client,
        &env,
        Method::GET,
        "/api/assistants/catalog/mine?limit=100",
        None,
        command,
    )
    .await?;
    let hub = request_json(
        client,
        &env,
        Method::GET,
        "/api/assistants/catalog/tjuae-hub?limit=100",
        None,
        command,
    )
    .await?;
    print_envelope(json!({ "mine": mine, "tjuaeHub": hub }), meta(None), command)
}

async fn run_assistant_get(client: &reqwest::Client) -> Result<(), ConfigError> {
    let command = "config assistants get";
    let env = ConfigEnv::from_env(command)?;
    let mut payload = read_stdin_payload(command)?;
    let (source, namespace, slug) = take_assistant_identity(&mut payload, command)?;
    let version = take_optional_string_field(&mut payload, "version");
    reject_remaining_fields(&payload, command)?;
    let mut path = assistant_catalog_resource_path(&source, &namespace, &slug);
    if let Some(version) = version {
        path.push_str("?version=");
        path.push_str(&encode_query_component(&version));
    }
    let data = request_json(client, &env, Method::GET, &path, None, command).await?;
    print_envelope(data, meta(None), command)
}

async fn run_assistant_create(client: &reqwest::Client) -> Result<(), ConfigError> {
    let command = "config assistants create";
    let env = ConfigEnv::from_env(command)?;
    let mut payload = read_stdin_payload(command)?;
    let slug = take_required_string_field(&mut payload, "slug", command)?;
    let name = take_required_string_field(&mut payload, "name", command)?;
    let description = take_optional_string_field(&mut payload, "description").unwrap_or_default();
    reject_remaining_fields(&payload, command)?;
    let data = request_json(
        client,
        &env,
        Method::POST,
        "/api/assistants/catalog/mine",
        Some(json!({ "slug": slug, "name": name, "description": description })),
        command,
    )
    .await?;
    print_envelope(data, meta(None), command)
}

async fn run_assistant_settings(client: &reqwest::Client) -> Result<(), ConfigError> {
    let command = "config assistants settings";
    let env = ConfigEnv::from_env(command)?;
    let mut payload = read_stdin_payload(command)?;
    let (source, namespace, slug) = take_assistant_identity(&mut payload, command)?;
    let name = take_required_string_field(&mut payload, "name", command)?;
    let description = take_optional_string_field(&mut payload, "description").unwrap_or_default();
    let avatar = take_optional_string_field(&mut payload, "avatar");
    let avatar_data_url = take_optional_string_field(&mut payload, "avatar_data_url");
    let defaults = take_required_value_field(&mut payload, "defaults", command)?;
    let recommended_prompts = take_required_value_field(&mut payload, "recommended_prompts", command)?;
    let rules = take_required_string_field(&mut payload, "rules", command)?;
    reject_remaining_fields(&payload, command)?;
    let path = assistant_catalog_operation_path(&source, &namespace, &slug, "settings");
    let data = request_json(
        client,
        &env,
        Method::PUT,
        &path,
        Some(json!({
            "name": name,
            "description": description,
            "avatar": avatar,
            "avatarDataUrl": avatar_data_url,
            "defaults": defaults,
            "recommendedPrompts": recommended_prompts,
            "rules": rules,
        })),
        command,
    )
    .await?;
    print_envelope(data, meta(None), command)
}

async fn run_assistant_delete(client: &reqwest::Client) -> Result<(), ConfigError> {
    let command = "config assistants delete";
    let env = ConfigEnv::from_env(command)?;
    let mut payload = read_stdin_payload(command)?;
    let (source, namespace, slug) = take_assistant_identity(&mut payload, command)?;
    reject_remaining_fields(&payload, command)?;
    let path = assistant_catalog_resource_path(&source, &namespace, &slug);
    let before = request_json(client, &env, Method::GET, &path, None, command).await?;
    let data = request_json(client, &env, Method::DELETE, &path, None, command).await?;
    let mut extra = Map::new();
    extra.insert("before".into(), redact_meta_value(before));
    print_envelope(data, meta_from_map(extra), command)
}

async fn run_assistant_copy(client: &reqwest::Client) -> Result<(), ConfigError> {
    let command = "config assistants copy";
    let env = ConfigEnv::from_env(command)?;
    let mut payload = read_stdin_payload(command)?;
    let (source, namespace, slug) = take_assistant_identity(&mut payload, command)?;
    let version = take_optional_string_field(&mut payload, "version");
    let target_slug = take_required_string_field(&mut payload, "target_slug", command)?;
    reject_remaining_fields(&payload, command)?;
    let path = assistant_catalog_operation_path(&source, &namespace, &slug, "copy-to-mine");
    let data = request_json(
        client,
        &env,
        Method::POST,
        &path,
        Some(json!({ "version": version, "targetSlug": target_slug })),
        command,
    )
    .await?;
    print_envelope(data, meta(None), command)
}

async fn run_assistant_preferences(client: &reqwest::Client) -> Result<(), ConfigError> {
    let command = "config assistants preferences";
    let env = ConfigEnv::from_env(command)?;
    let mut payload = read_stdin_payload(command)?;
    let (source, namespace, slug) = take_assistant_identity(&mut payload, command)?;
    let selected_version = take_optional_string_field(&mut payload, "selected_version");
    let object = payload.as_object_mut().ok_or_else(|| {
        ConfigError::new(ConfigErrorCode::PayloadInvalid, command, "JSON 载荷必须是对象").field("field", "stdin")
    })?;
    let follow_latest = take_required_bool_field(object, "follow_latest", command)?;
    let enabled = take_required_bool_field(object, "enabled", command)?;
    let sort_order = object
        .remove("sort_order")
        .and_then(|value| value.as_i64())
        .map(|value| value as i32);
    reject_remaining_fields(&payload, command)?;
    let path = assistant_catalog_resource_path(&source, &namespace, &slug);
    let data = request_json(
        client,
        &env,
        Method::PATCH,
        &path,
        Some(json!({
            "selectedVersion": selected_version,
            "followLatest": follow_latest,
            "enabled": enabled,
            "sortOrder": sort_order,
        })),
        command,
    )
    .await?;
    print_envelope(data, meta(None), command)
}

async fn run_assistant_file(client: &reqwest::Client, action: ConfigAssistantFileCommand) -> Result<(), ConfigError> {
    let action_name = match action {
        ConfigAssistantFileCommand::Read => "read",
        ConfigAssistantFileCommand::Write => "write",
    };
    let command = format!("config assistants file {action_name}");
    let env = ConfigEnv::from_env(&command)?;
    let mut payload = read_stdin_payload(&command)?;
    let (source, namespace, slug) = take_assistant_identity(&mut payload, &command)?;
    let file_path = take_required_string_field(&mut payload, "path", &command)?;
    let version = take_optional_string_field(&mut payload, "version");
    let resource = assistant_catalog_operation_path(&source, &namespace, &slug, "file");
    match action {
        ConfigAssistantFileCommand::Read => {
            reject_remaining_fields(&payload, &command)?;
            let mut path = format!("{resource}?path={}", encode_query_component(&file_path));
            if let Some(version) = version {
                path.push_str("&version=");
                path.push_str(&encode_query_component(&version));
            }
            let data = request_json(client, &env, Method::GET, &path, None, &command).await?;
            print_envelope(data, meta(None), &command)
        }
        ConfigAssistantFileCommand::Write => {
            if version.is_some() {
                return Err(ConfigError::new(
                    ConfigErrorCode::PayloadInvalid,
                    &command,
                    "历史版本只读，写入时不能指定 version",
                ));
            }
            let content = take_required_value_field(&mut payload, "content", &command)?
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| {
                    ConfigError::new(ConfigErrorCode::PayloadInvalid, &command, "content 必须是字符串")
                        .field("field", "content")
                })?;
            reject_remaining_fields(&payload, &command)?;
            let data = request_json(
                client,
                &env,
                Method::PUT,
                &resource,
                Some(json!({ "path": file_path, "content": content })),
                &command,
            )
            .await?;
            print_envelope(data, meta(None), &command)
        }
    }
}

async fn run_assistant_prepare(client: &reqwest::Client) -> Result<(), ConfigError> {
    let command = "config assistants prepare";
    let env = ConfigEnv::from_env(command)?;
    let mut payload = read_stdin_payload(command)?;
    let (source, namespace, slug) = take_assistant_identity(&mut payload, command)?;
    let version = take_optional_string_field(&mut payload, "version");
    reject_remaining_fields(&payload, command)?;
    let path = assistant_catalog_operation_path(&source, &namespace, &slug, "activation/prepare");
    let data = request_json(
        client,
        &env,
        Method::POST,
        &path,
        Some(json!({ "version": version })),
        command,
    )
    .await?;
    print_envelope(data, meta(None), command)
}

async fn run_assistant_activate(client: &reqwest::Client) -> Result<(), ConfigError> {
    let command = "config assistants activate";
    let env = ConfigEnv::from_env(command)?;
    let mut payload = read_stdin_payload(command)?;
    let (source, namespace, slug) = take_assistant_identity(&mut payload, command)?;
    let plan_id = take_required_string_field(&mut payload, "plan_id", command)?;
    let fingerprint = take_required_string_field(&mut payload, "fingerprint", command)?;
    let confirmed_groups = take_required_value_field(&mut payload, "confirmed_groups", command)?;
    let choices = take_required_value_field(&mut payload, "choices", command)?;
    if !confirmed_groups.is_array() || !choices.is_array() {
        return Err(ConfigError::new(
            ConfigErrorCode::PayloadInvalid,
            command,
            "confirmed_groups 与 choices 必须是数组",
        ));
    }
    reject_remaining_fields(&payload, command)?;
    let path = assistant_catalog_operation_path(&source, &namespace, &slug, "activation/commit");
    let data = request_json(
        client,
        &env,
        Method::POST,
        &path,
        Some(json!({
            "planId": plan_id,
            "fingerprint": fingerprint,
            "confirmedGroups": confirmed_groups,
            "choices": choices,
        })),
        command,
    )
    .await?;
    print_envelope(data, meta(None), command)
}

async fn run_assistant_export(client: &reqwest::Client) -> Result<(), ConfigError> {
    let command = "config assistants export";
    let env = ConfigEnv::from_env(command)?;
    let mut payload = read_stdin_payload(command)?;
    let (source, namespace, slug) = take_assistant_identity(&mut payload, command)?;
    let version = take_optional_string_field(&mut payload, "version");
    let output_path = take_required_string_field(&mut payload, "output_path", command)?;
    reject_remaining_fields(&payload, command)?;
    let path = assistant_catalog_operation_path(&source, &namespace, &slug, "export");
    let data = request_json(
        client,
        &env,
        Method::POST,
        &path,
        Some(json!({ "version": version, "outputPath": output_path })),
        command,
    )
    .await?;
    print_envelope(data, meta(None), command)
}

async fn run_assistant_publish(client: &reqwest::Client) -> Result<(), ConfigError> {
    let command = "config assistants publish";
    let env = ConfigEnv::from_env(command)?;
    let mut payload = read_stdin_payload(command)?;
    let (source, namespace, slug) = take_assistant_identity(&mut payload, command)?;
    let message = take_required_string_field(&mut payload, "message", command)?;
    reject_remaining_fields(&payload, command)?;
    let path = assistant_catalog_operation_path(&source, &namespace, &slug, "publish");
    let data = request_json(
        client,
        &env,
        Method::POST,
        &path,
        Some(json!({ "message": message })),
        command,
    )
    .await?;
    print_envelope(data, meta(None), command)
}

fn take_assistant_identity(payload: &mut Value, command: &str) -> Result<(String, String, String), ConfigError> {
    let source = take_required_string_field(payload, "source", command)?;
    let namespace = take_optional_string_field(payload, "namespace").unwrap_or_default();
    let slug = take_required_string_field(payload, "slug", command)?;
    Ok((source, namespace, slug))
}

fn take_required_value_field(payload: &mut Value, field: &'static str, command: &str) -> Result<Value, ConfigError> {
    payload
        .as_object_mut()
        .and_then(|object| object.remove(field))
        .ok_or_else(|| {
            ConfigError::new(ConfigErrorCode::PayloadInvalid, command, "missing required field").field("field", field)
        })
}

fn assistant_catalog_resource_path(source: &str, namespace: &str, slug: &str) -> String {
    let namespace = if namespace.is_empty() { "~" } else { namespace };
    format!(
        "/api/assistants/catalog/{}/{}/{}",
        encode_path_segment(source),
        encode_path_segment(namespace),
        encode_path_segment(slug)
    )
}

fn assistant_catalog_operation_path(source: &str, namespace: &str, slug: &str, operation: &str) -> String {
    format!(
        "{}/{}",
        assistant_catalog_resource_path(source, namespace, slug),
        operation
    )
}

async fn run_skills(client: &reqwest::Client, args: ConfigSkillsArgs) -> Result<(), ConfigError> {
    match args.command {
        ConfigSkillsCommand::List => {
            let command = "config skills list";
            let env = ConfigEnv::from_env(command)?;
            let data = request_json(client, &env, Method::GET, "/api/skills/catalog", None, command).await?;
            print_envelope(data, meta(None), command)
        }
        ConfigSkillsCommand::Create => {
            run_payload_request_with_collection_readback(
                client,
                "config skills create",
                Method::POST,
                "/api/skills/create",
                "/api/skills/catalog",
                false,
            )
            .await
        }
        ConfigSkillsCommand::Import => run_skill_import(client).await,
        ConfigSkillsCommand::Delete => run_skill_delete(client).await,
        ConfigSkillsCommand::Copy => run_skill_copy(client).await,
        ConfigSkillsCommand::Preferences => run_skill_preferences(client).await,
    }
}

async fn run_skill_import(client: &reqwest::Client) -> Result<(), ConfigError> {
    let command = "config skills import";
    let env = ConfigEnv::from_env(command)?;
    let mut payload = read_stdin_payload(command)?;
    let archive_path = take_required_string_field(&mut payload, "archive_path", command)?;
    reject_remaining_fields(&payload, command)?;
    let before = request_json(client, &env, Method::GET, "/api/skills/catalog", None, command).await?;
    let data = request_json(
        client,
        &env,
        Method::POST,
        "/api/skills/import",
        Some(json!({ "archivePath": archive_path })),
        command,
    )
    .await?;
    let after = request_json(client, &env, Method::GET, "/api/skills/catalog", None, command).await?;
    let mut extra = Map::new();
    extra.insert("before".into(), redact_meta_value(before));
    extra.insert("after".into(), redact_meta_value(after));
    print_envelope(data, meta_from_map(extra), command)
}

async fn run_skill_copy(client: &reqwest::Client) -> Result<(), ConfigError> {
    let command = "config skills copy";
    let env = ConfigEnv::from_env(command)?;
    let mut payload = read_stdin_payload(command)?;
    let source = take_required_string_field(&mut payload, "source", command)?;
    let namespace = take_required_string_field(&mut payload, "namespace", command)?;
    let slug = take_required_string_field(&mut payload, "slug", command)?;
    let version = take_required_string_field(&mut payload, "version", command)?;
    let target_slug = take_required_string_field(&mut payload, "target_slug", command)?;
    reject_remaining_fields(&payload, command)?;
    let before = request_json(client, &env, Method::GET, "/api/skills/catalog", None, command).await?;
    let path = skill_catalog_operation_path(&source, &namespace, &slug, "copy-to-mine");
    let data = request_json(
        client,
        &env,
        Method::POST,
        &path,
        Some(json!({ "version": version, "targetSlug": target_slug })),
        command,
    )
    .await?;
    let after = request_json(client, &env, Method::GET, "/api/skills/catalog", None, command).await?;
    let mut extra = Map::new();
    extra.insert("before".into(), redact_meta_value(before));
    extra.insert("after".into(), redact_meta_value(after));
    print_envelope(data, meta_from_map(extra), command)
}

async fn run_skill_preferences(client: &reqwest::Client) -> Result<(), ConfigError> {
    let command = "config skills preferences";
    let env = ConfigEnv::from_env(command)?;
    let mut payload = read_stdin_payload(command)?;
    let source = take_required_string_field(&mut payload, "source", command)?;
    let namespace = take_required_string_field(&mut payload, "namespace", command)?;
    let slug = take_required_string_field(&mut payload, "slug", command)?;
    let selected_version = take_required_string_field(&mut payload, "selected_version", command)?;
    let object = payload.as_object_mut().ok_or_else(|| {
        ConfigError::new(ConfigErrorCode::PayloadInvalid, command, "JSON 载荷必须是对象").field("field", "stdin")
    })?;
    let enabled = take_required_bool_field(object, "enabled", command)?;
    let auto_inject = take_required_bool_field(object, "auto_inject", command)?;
    let follow_latest = take_required_bool_field(object, "follow_latest", command)?;
    reject_remaining_fields(&payload, command)?;
    let before = request_json(client, &env, Method::GET, "/api/skills/catalog", None, command).await?;
    let path = skill_catalog_operation_path(&source, &namespace, &slug, "preferences");
    let data = request_json(
        client,
        &env,
        Method::PUT,
        &path,
        Some(json!({
            "enabled": enabled,
            "autoInject": auto_inject,
            "selectedVersion": selected_version,
            "followLatest": follow_latest,
        })),
        command,
    )
    .await?;
    let after = request_json(client, &env, Method::GET, "/api/skills/catalog", None, command).await?;
    let mut extra = Map::new();
    extra.insert("before".into(), redact_meta_value(before));
    extra.insert("after".into(), redact_meta_value(after));
    print_envelope(data, meta_from_map(extra), command)
}

async fn run_skill_delete(client: &reqwest::Client) -> Result<(), ConfigError> {
    let command = "config skills delete";
    let env = ConfigEnv::from_env(command)?;
    let mut payload = read_stdin_payload(command)?;
    let source = take_required_string_field(&mut payload, "source", command)?;
    let namespace = take_required_string_field(&mut payload, "namespace", command)?;
    let slug = take_required_string_field(&mut payload, "slug", command)?;
    reject_remaining_fields(&payload, command)?;
    let path = skill_catalog_resource_path(&source, &namespace, &slug);
    let before = request_json(client, &env, Method::GET, "/api/skills/catalog", None, command).await?;
    let data = request_json(client, &env, Method::DELETE, &path, None, command).await?;
    let after = request_json(client, &env, Method::GET, "/api/skills/catalog", None, command).await?;
    let mut extra = Map::new();
    extra.insert("before".into(), redact_meta_value(before));
    extra.insert("after".into(), redact_meta_value(after));
    print_envelope(data, meta_from_map(extra), command)
}

fn skill_catalog_resource_path(source: &str, namespace: &str, slug: &str) -> String {
    format!(
        "/api/skills/catalog/{}/{}/{}",
        encode_path_segment(source),
        encode_path_segment(namespace),
        encode_path_segment(slug)
    )
}

fn skill_catalog_operation_path(source: &str, namespace: &str, slug: &str, operation: &str) -> String {
    format!("{}/{}", skill_catalog_resource_path(source, namespace, slug), operation)
}

async fn run_mcp(client: &reqwest::Client, args: ConfigMcpArgs) -> Result<(), ConfigError> {
    match args.command {
        ConfigMcpCommand::Servers(args) => match args.command {
            ConfigMcpServersCommand::List => {
                run_no_input_request(client, "config mcp servers list", Method::GET, "/api/mcp/servers", true).await
            }
            ConfigMcpServersCommand::Get => {
                run_id_no_body_request(
                    client,
                    "config mcp servers get",
                    Method::GET,
                    "/api/mcp/servers",
                    "",
                    "server_id",
                    true,
                )
                .await
            }
            ConfigMcpServersCommand::Create => {
                run_payload_request_with_collection_readback(
                    client,
                    "config mcp servers create",
                    Method::POST,
                    "/api/mcp/servers",
                    "/api/mcp/servers",
                    true,
                )
                .await
            }
            ConfigMcpServersCommand::Update => run_mcp_server_update(client).await,
            ConfigMcpServersCommand::Delete => {
                run_id_no_body_request_with_collection_readback(
                    client,
                    "config mcp servers delete",
                    Method::DELETE,
                    IdRoute::new("/api/mcp/servers", "", "server_id"),
                    "/api/mcp/servers",
                    true,
                )
                .await
            }
            ConfigMcpServersCommand::Toggle => {
                run_id_no_body_request_with_resource_readback(
                    client,
                    "config mcp servers toggle",
                    Method::POST,
                    IdRoute::new("/api/mcp/servers", "/toggle", "server_id"),
                    IdRoute::new("/api/mcp/servers", "", "server_id"),
                    true,
                )
                .await
            }
            ConfigMcpServersCommand::Import => {
                run_payload_request_with_collection_readback(
                    client,
                    "config mcp servers import",
                    Method::POST,
                    "/api/mcp/servers/import",
                    "/api/mcp/servers",
                    true,
                )
                .await
            }
        },
        ConfigMcpCommand::TestConnection => {
            run_payload_request(
                client,
                "config mcp test-connection",
                Method::POST,
                "/api/mcp/test-connection",
                true,
            )
            .await
        }
        ConfigMcpCommand::AgentConfigs => {
            run_no_input_request(
                client,
                "config mcp agent-configs",
                Method::GET,
                "/api/mcp/agent-configs",
                true,
            )
            .await
        }
        ConfigMcpCommand::Oauth(args) => match args.command {
            ConfigMcpOauthCommand::CheckStatus => {
                run_payload_request(
                    client,
                    "config mcp oauth check-status",
                    Method::POST,
                    "/api/mcp/oauth/check-status",
                    true,
                )
                .await
            }
            ConfigMcpOauthCommand::Login => {
                run_payload_request_with_body_readback(
                    client,
                    "config mcp oauth login",
                    Method::POST,
                    "/api/mcp/oauth/login",
                    "/api/mcp/oauth/check-status",
                    true,
                )
                .await
            }
            ConfigMcpOauthCommand::Logout => {
                run_payload_request_with_body_readback(
                    client,
                    "config mcp oauth logout",
                    Method::POST,
                    "/api/mcp/oauth/logout",
                    "/api/mcp/oauth/check-status",
                    true,
                )
                .await
            }
            ConfigMcpOauthCommand::Authenticated => {
                run_no_input_request(
                    client,
                    "config mcp oauth authenticated",
                    Method::GET,
                    "/api/mcp/oauth/authenticated",
                    true,
                )
                .await
            }
        },
    }
}

async fn run_mcp_server_update(client: &reqwest::Client) -> Result<(), ConfigError> {
    let command = "config mcp servers update";
    let env = ConfigEnv::from_env(command)?;
    let mut payload = read_stdin_payload(command)?;
    let mut selectors = SelectorMeta::default();
    resolve_top_level_selectors(client, &env, command, &mut payload, &mut selectors).await?;
    let id = take_required_string_field(&mut payload, "server_id", command)?;
    let path = format!("/api/mcp/servers/{}", encode_path_segment(&id));
    let before = request_json(client, &env, Method::GET, &path, None, command).await?;
    let data = request_json(client, &env, Method::PUT, &path, Some(payload), command).await?;
    let after = request_json(client, &env, Method::GET, &path, None, command).await?;
    let mut extra = selectors.into_map();
    extra.insert("before".into(), redact_meta_value(before));
    extra.insert("after".into(), redact_meta_value(after));
    print_envelope(redact_meta_value(data), meta_from_map(extra), command)
}

async fn run_providers(client: &reqwest::Client, args: ConfigProvidersArgs) -> Result<(), ConfigError> {
    match args.command {
        ConfigProvidersCommand::List => {
            run_no_input_request(client, "config providers list", Method::GET, "/api/providers", true).await
        }
        ConfigProvidersCommand::Create => {
            run_payload_request_with_collection_readback(
                client,
                "config providers create",
                Method::POST,
                "/api/providers",
                "/api/providers",
                true,
            )
            .await
        }
        ConfigProvidersCommand::Update => {
            run_id_payload_request_with_collection_readback(
                client,
                "config providers update",
                Method::PUT,
                IdRoute::new("/api/providers", "", "provider_id"),
                "/api/providers",
                true,
            )
            .await
        }
        ConfigProvidersCommand::Delete => {
            run_id_no_body_request_with_collection_readback(
                client,
                "config providers delete",
                Method::DELETE,
                IdRoute::new("/api/providers", "", "provider_id"),
                "/api/providers",
                true,
            )
            .await
        }
        ConfigProvidersCommand::DetectProtocol => {
            run_payload_request(
                client,
                "config providers detect-protocol",
                Method::POST,
                "/api/providers/detect-protocol",
                true,
            )
            .await
        }
        ConfigProvidersCommand::FetchModels => {
            run_payload_request(
                client,
                "config providers fetch-models",
                Method::POST,
                "/api/providers/fetch-models",
                true,
            )
            .await
        }
        ConfigProvidersCommand::Models(args) => match args.command {
            ConfigProviderModelsCommand::Fetch => {
                run_id_payload_request_with_collection_readback(
                    client,
                    "config providers models fetch",
                    Method::POST,
                    IdRoute::new("/api/providers", "/models", "provider_id"),
                    "/api/providers",
                    true,
                )
                .await
            }
        },
        ConfigProvidersCommand::HealthCheck => {
            run_payload_request(
                client,
                "config providers health-check",
                Method::POST,
                "/api/agents/provider-health-check",
                true,
            )
            .await
        }
    }
}

async fn run_settings(client: &reqwest::Client, args: ConfigSettingsArgs) -> Result<(), ConfigError> {
    match args.command {
        ConfigSettingsCommand::Get => {
            run_no_input_request(client, "config settings get", Method::GET, "/api/settings", false).await
        }
        ConfigSettingsCommand::Patch => run_settings_patch(client).await,
        ConfigSettingsCommand::Client(args) => match args.command {
            ConfigSettingsClientCommand::Get => {
                run_no_input_request(
                    client,
                    "config settings client get",
                    Method::GET,
                    "/api/settings/client",
                    true,
                )
                .await
            }
            ConfigSettingsClientCommand::Put => run_settings_client_put(client).await,
        },
    }
}

async fn run_settings_patch(client: &reqwest::Client) -> Result<(), ConfigError> {
    let command = "config settings patch";
    let env = ConfigEnv::from_env(command)?;
    let mut payload = read_stdin_payload(command)?;
    let mut selectors = SelectorMeta::default();
    resolve_top_level_selectors(client, &env, command, &mut payload, &mut selectors).await?;
    let before = request_json(client, &env, Method::GET, "/api/settings", None, command).await?;
    let data = request_json(client, &env, Method::PATCH, "/api/settings", Some(payload), command).await?;
    let after = request_json(client, &env, Method::GET, "/api/settings", None, command).await?;
    let mut extra = selectors.into_map();
    extra.insert("before".into(), redact_meta_value(before));
    extra.insert("after".into(), redact_meta_value(after));
    print_envelope(redact_meta_value(data), meta_from_map(extra), command)
}

async fn run_settings_client_put(client: &reqwest::Client) -> Result<(), ConfigError> {
    let command = "config settings client put";
    let env = ConfigEnv::from_env(command)?;
    let mut payload = read_stdin_payload(command)?;
    let mut selectors = SelectorMeta::default();
    resolve_top_level_selectors(client, &env, command, &mut payload, &mut selectors).await?;
    let before = request_json(client, &env, Method::GET, "/api/settings/client", None, command).await?;
    let data = request_json(
        client,
        &env,
        Method::PUT,
        "/api/settings/client",
        Some(payload),
        command,
    )
    .await?;
    let after = request_json(client, &env, Method::GET, "/api/settings/client", None, command).await?;
    let mut extra = selectors.into_map();
    extra.insert("before".into(), redact_meta_value(before));
    extra.insert("after".into(), redact_meta_value(after));
    print_envelope(redact_meta_value(data), meta_from_map(extra), command)
}

async fn run_agents(client: &reqwest::Client, args: ConfigAgentsArgs) -> Result<(), ConfigError> {
    match args.command {
        ConfigAgentsCommand::List => {
            run_no_input_request(
                client,
                "config agents list",
                Method::GET,
                "/api/agents/management",
                true,
            )
            .await
        }
        ConfigAgentsCommand::Enable => {
            run_id_payload_request_with_collection_readback(
                client,
                "config agents enable",
                Method::PATCH,
                IdRoute::new("/api/agents", "/enabled", "agent_id"),
                "/api/agents/management",
                true,
            )
            .await
        }
        ConfigAgentsCommand::Overrides(args) => match args.command {
            ConfigAgentOverridesCommand::Get => {
                run_id_no_body_request(
                    client,
                    "config agents overrides get",
                    Method::GET,
                    "/api/agents",
                    "/overrides",
                    "agent_id",
                    true,
                )
                .await
            }
            ConfigAgentOverridesCommand::Set => {
                run_id_payload_request_with_resource_readback(
                    client,
                    "config agents overrides set",
                    Method::PUT,
                    IdRoute::new("/api/agents", "/overrides", "agent_id"),
                    IdRoute::new("/api/agents", "/overrides", "agent_id"),
                    true,
                )
                .await
            }
        },
        ConfigAgentsCommand::Custom(args) => match args.command {
            ConfigAgentCustomCommand::Create => {
                run_payload_request_with_collection_readback(
                    client,
                    "config agents custom create",
                    Method::POST,
                    "/api/agents/custom",
                    "/api/agents/management",
                    true,
                )
                .await
            }
            ConfigAgentCustomCommand::Update => {
                run_id_payload_request_with_collection_readback(
                    client,
                    "config agents custom update",
                    Method::PUT,
                    IdRoute::new("/api/agents/custom", "", "agent_id"),
                    "/api/agents/management",
                    true,
                )
                .await
            }
            ConfigAgentCustomCommand::Delete => {
                run_id_no_body_request_with_collection_readback(
                    client,
                    "config agents custom delete",
                    Method::DELETE,
                    IdRoute::new("/api/agents/custom", "", "agent_id"),
                    "/api/agents/management",
                    true,
                )
                .await
            }
            ConfigAgentCustomCommand::TryConnect => {
                run_payload_request(
                    client,
                    "config agents custom try-connect",
                    Method::POST,
                    "/api/agents/custom/try-connect",
                    true,
                )
                .await
            }
        },
    }
}

async fn run_cron(client: &reqwest::Client, args: ConfigCronArgs) -> Result<(), ConfigError> {
    match args.command {
        ConfigCronCommand::Jobs(args) => run_cron_jobs(client, args).await,
        ConfigCronCommand::Current(args) => run_cron_current(client, args).await,
    }
}

async fn run_cron_jobs(client: &reqwest::Client, args: ConfigCronJobsArgs) -> Result<(), ConfigError> {
    match args.command {
        ConfigCronJobsCommand::List => {
            run_no_input_request(client, "config cron jobs list", Method::GET, "/api/cron/jobs", false).await
        }
        ConfigCronJobsCommand::Get => {
            run_id_no_body_request(
                client,
                "config cron jobs get",
                Method::GET,
                "/api/cron/jobs",
                "",
                "job_id",
                false,
            )
            .await
        }
        ConfigCronJobsCommand::Create => {
            run_payload_request_with_collection_readback(
                client,
                "config cron jobs create",
                Method::POST,
                "/api/cron/jobs",
                "/api/cron/jobs",
                false,
            )
            .await
        }
        ConfigCronJobsCommand::Update => {
            run_id_payload_request_with_resource_readback(
                client,
                "config cron jobs update",
                Method::PUT,
                IdRoute::new("/api/cron/jobs", "", "job_id"),
                IdRoute::new("/api/cron/jobs", "", "job_id"),
                false,
            )
            .await
        }
        ConfigCronJobsCommand::Delete => {
            run_id_no_body_request_with_collection_readback(
                client,
                "config cron jobs delete",
                Method::DELETE,
                IdRoute::new("/api/cron/jobs", "", "job_id"),
                "/api/cron/jobs",
                false,
            )
            .await
        }
        ConfigCronJobsCommand::Run => {
            run_id_no_body_request(
                client,
                "config cron jobs run",
                Method::POST,
                "/api/cron/jobs",
                "/run",
                "job_id",
                false,
            )
            .await
        }
        ConfigCronJobsCommand::Skill(args) => match args.command {
            ConfigCronJobSkillCommand::Get => {
                run_id_no_body_request(
                    client,
                    "config cron jobs skill get",
                    Method::GET,
                    "/api/cron/jobs",
                    "/skill",
                    "job_id",
                    false,
                )
                .await
            }
            ConfigCronJobSkillCommand::Save => {
                run_id_payload_request_with_resource_readback(
                    client,
                    "config cron jobs skill save",
                    Method::POST,
                    IdRoute::new("/api/cron/jobs", "/skill", "job_id"),
                    IdRoute::new("/api/cron/jobs", "/skill", "job_id"),
                    false,
                )
                .await
            }
            ConfigCronJobSkillCommand::Delete => {
                run_id_no_body_request_with_resource_readback(
                    client,
                    "config cron jobs skill delete",
                    Method::DELETE,
                    IdRoute::new("/api/cron/jobs", "/skill", "job_id"),
                    IdRoute::new("/api/cron/jobs", "/skill", "job_id"),
                    false,
                )
                .await
            }
        },
    }
}

async fn run_cron_current(client: &reqwest::Client, args: ConfigCronCurrentArgs) -> Result<(), ConfigError> {
    match args.command {
        ConfigCronCurrentCommand::List => {
            let command = "config cron current list";
            let env = ConfigEnv::from_env(command)?;
            let data = request_json(
                client,
                &env,
                Method::GET,
                "/api/internal/conversation-cron/list",
                None,
                command,
            )
            .await?;
            print_envelope(data, meta(None), command)
        }
        ConfigCronCurrentCommand::Create => {
            run_payload_request_with_collection_readback(
                client,
                "config cron current create",
                Method::POST,
                "/api/internal/conversation-cron/create",
                "/api/internal/conversation-cron/list",
                false,
            )
            .await
        }
        ConfigCronCurrentCommand::Update => run_cron_current_update(client).await,
    }
}

async fn run_cron_current_update(client: &reqwest::Client) -> Result<(), ConfigError> {
    let command = "config cron current update";
    let env = ConfigEnv::from_env(command)?;
    let mut payload = read_stdin_payload(command)?;
    let mut selectors = SelectorMeta::default();
    resolve_top_level_selectors(client, &env, command, &mut payload, &mut selectors).await?;
    let job_id = take_required_string_field(&mut payload, "job_id", command)?;
    let path = format!("/api/internal/conversation-cron/jobs/{}", encode_path_segment(&job_id));
    let before = request_json(
        client,
        &env,
        Method::GET,
        "/api/internal/conversation-cron/list",
        None,
        command,
    )
    .await?;
    let data = request_json(client, &env, Method::PUT, &path, Some(payload), command).await?;
    let after = request_json(
        client,
        &env,
        Method::GET,
        "/api/internal/conversation-cron/list",
        None,
        command,
    )
    .await?;
    let mut extra = selectors.into_map();
    extra.insert("before".into(), redact_meta_value(before));
    extra.insert("after".into(), redact_meta_value(after));
    print_envelope(data, meta_from_map(extra), command)
}

async fn run_no_input_request(
    client: &reqwest::Client,
    command: &'static str,
    method: Method,
    path: &'static str,
    redact_output: bool,
) -> Result<(), ConfigError> {
    let env = ConfigEnv::from_env(command)?;
    let data = request_json(client, &env, method, path, None, command).await?;
    print_config_output(data, meta(None), command, redact_output)
}

async fn run_payload_request(
    client: &reqwest::Client,
    command: &'static str,
    method: Method,
    path: &'static str,
    redact_output: bool,
) -> Result<(), ConfigError> {
    let env = ConfigEnv::from_env(command)?;
    let mut payload = read_stdin_payload(command)?;
    let mut selectors = SelectorMeta::default();
    resolve_top_level_selectors(client, &env, command, &mut payload, &mut selectors).await?;
    let data = request_json(client, &env, method, path, Some(payload), command).await?;
    print_config_output(data, meta(Some(selectors)), command, redact_output)
}

async fn run_payload_request_with_collection_readback(
    client: &reqwest::Client,
    command: &'static str,
    method: Method,
    path: &'static str,
    collection_path: &'static str,
    redact_output: bool,
) -> Result<(), ConfigError> {
    let env = ConfigEnv::from_env(command)?;
    let mut payload = read_stdin_payload(command)?;
    let mut selectors = SelectorMeta::default();
    resolve_top_level_selectors(client, &env, command, &mut payload, &mut selectors).await?;
    let before = request_json(client, &env, Method::GET, collection_path, None, command).await?;
    let data = request_json(client, &env, method, path, Some(payload), command).await?;
    let after = request_json(client, &env, Method::GET, collection_path, None, command).await?;
    print_config_output(
        data,
        readback_meta(selectors.into_map(), before, after),
        command,
        redact_output,
    )
}

async fn run_payload_request_with_body_readback(
    client: &reqwest::Client,
    command: &'static str,
    method: Method,
    path: &'static str,
    read_path: &'static str,
    redact_output: bool,
) -> Result<(), ConfigError> {
    let env = ConfigEnv::from_env(command)?;
    let mut payload = read_stdin_payload(command)?;
    let mut selectors = SelectorMeta::default();
    resolve_top_level_selectors(client, &env, command, &mut payload, &mut selectors).await?;
    let before = request_json(client, &env, Method::POST, read_path, Some(payload.clone()), command).await?;
    let data = request_json(client, &env, method, path, Some(payload.clone()), command).await?;
    let after = request_json(client, &env, Method::POST, read_path, Some(payload), command).await?;
    print_config_output(
        data,
        readback_meta(selectors.into_map(), before, after),
        command,
        redact_output,
    )
}

async fn run_id_no_body_request(
    client: &reqwest::Client,
    command: &'static str,
    method: Method,
    path_prefix: &'static str,
    path_suffix: &'static str,
    id_field: &'static str,
    redact_output: bool,
) -> Result<(), ConfigError> {
    let env = ConfigEnv::from_env(command)?;
    let payload = read_stdin_payload(command)?;
    let id = required_string_field(&payload, id_field, command)?;
    let path = id_path(path_prefix, &id, path_suffix);
    let data = request_json(client, &env, method, &path, None, command).await?;
    print_config_output(data, meta(None), command, redact_output)
}

async fn run_id_no_body_request_with_collection_readback(
    client: &reqwest::Client,
    command: &'static str,
    method: Method,
    route: IdRoute,
    collection_path: &'static str,
    redact_output: bool,
) -> Result<(), ConfigError> {
    let env = ConfigEnv::from_env(command)?;
    let payload = read_stdin_payload(command)?;
    let id = required_string_field(&payload, route.id_field, command)?;
    let path = route.path(&id);
    let before = request_json(client, &env, Method::GET, collection_path, None, command).await?;
    let data = request_json(client, &env, method, &path, None, command).await?;
    let after = request_json(client, &env, Method::GET, collection_path, None, command).await?;
    print_config_output(data, readback_meta(Map::new(), before, after), command, redact_output)
}

async fn run_id_no_body_request_with_resource_readback(
    client: &reqwest::Client,
    command: &'static str,
    method: Method,
    write_route: IdRoute,
    read_route: IdRoute,
    redact_output: bool,
) -> Result<(), ConfigError> {
    let env = ConfigEnv::from_env(command)?;
    let payload = read_stdin_payload(command)?;
    let id = required_string_field(&payload, write_route.id_field, command)?;
    let path = write_route.path(&id);
    let read_path = read_route.path(&id);
    let before = request_json(client, &env, Method::GET, &read_path, None, command).await?;
    let data = request_json(client, &env, method, &path, None, command).await?;
    let after = request_json(client, &env, Method::GET, &read_path, None, command).await?;
    print_config_output(data, readback_meta(Map::new(), before, after), command, redact_output)
}

async fn run_id_payload_request_with_collection_readback(
    client: &reqwest::Client,
    command: &'static str,
    method: Method,
    route: IdRoute,
    collection_path: &'static str,
    redact_output: bool,
) -> Result<(), ConfigError> {
    let env = ConfigEnv::from_env(command)?;
    let mut payload = read_stdin_payload(command)?;
    let mut selectors = SelectorMeta::default();
    resolve_top_level_selectors(client, &env, command, &mut payload, &mut selectors).await?;
    let id = take_required_string_field(&mut payload, route.id_field, command)?;
    let path = route.path(&id);
    let before = request_json(client, &env, Method::GET, collection_path, None, command).await?;
    let data = request_json(client, &env, method, &path, Some(payload), command).await?;
    let after = request_json(client, &env, Method::GET, collection_path, None, command).await?;
    print_config_output(
        data,
        readback_meta(selectors.into_map(), before, after),
        command,
        redact_output,
    )
}

async fn run_id_payload_request_with_resource_readback(
    client: &reqwest::Client,
    command: &'static str,
    method: Method,
    write_route: IdRoute,
    read_route: IdRoute,
    redact_output: bool,
) -> Result<(), ConfigError> {
    let env = ConfigEnv::from_env(command)?;
    let mut payload = read_stdin_payload(command)?;
    let mut selectors = SelectorMeta::default();
    resolve_top_level_selectors(client, &env, command, &mut payload, &mut selectors).await?;
    let id = take_required_string_field(&mut payload, write_route.id_field, command)?;
    let path = write_route.path(&id);
    let read_path = read_route.path(&id);
    let before = request_json(client, &env, Method::GET, &read_path, None, command).await?;
    let data = request_json(client, &env, method, &path, Some(payload), command).await?;
    let after = request_json(client, &env, Method::GET, &read_path, None, command).await?;
    print_config_output(
        data,
        readback_meta(selectors.into_map(), before, after),
        command,
        redact_output,
    )
}

fn id_path(path_prefix: &str, id: &str, path_suffix: &str) -> String {
    format!("{}/{}{}", path_prefix, encode_path_segment(id), path_suffix)
}

#[derive(Clone, Copy)]
struct IdRoute {
    path_prefix: &'static str,
    path_suffix: &'static str,
    id_field: &'static str,
}

impl IdRoute {
    const fn new(path_prefix: &'static str, path_suffix: &'static str, id_field: &'static str) -> Self {
        Self {
            path_prefix,
            path_suffix,
            id_field,
        }
    }

    fn path(self, id: &str) -> String {
        id_path(self.path_prefix, id, self.path_suffix)
    }
}

fn readback_meta(mut extra: Map<String, Value>, before: Value, after: Value) -> Value {
    extra.insert("before".into(), redact_meta_value(before));
    extra.insert("after".into(), redact_meta_value(after));
    meta_from_map(extra)
}

fn print_config_output(data: Value, meta: Value, command: &str, redact_output: bool) -> Result<(), ConfigError> {
    let data = if redact_output { redact_meta_value(data) } else { data };
    print_envelope(data, meta, command)
}

#[derive(Debug, Clone)]
struct ConfigEnv {
    base_url: String,
    conversation_id: String,
    user_id: String,
}

impl ConfigEnv {
    fn from_env(command: &str) -> Result<Self, ConfigError> {
        Ok(Self {
            base_url: required_env(command, ENV_BASE_URL)?.trim_end_matches('/').to_owned(),
            conversation_id: required_env(command, ENV_CONVERSATION_ID)?,
            user_id: required_env(command, ENV_USER_ID)?,
        })
    }
}

fn required_env(command: &str, name: &'static str) -> Result<String, ConfigError> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            ConfigError::new(
                ConfigErrorCode::EnvMissing,
                command,
                "missing required environment variable",
            )
            .field("field", name)
        })
}

async fn fetch_current_conversation(
    client: &reqwest::Client,
    env: &ConfigEnv,
    command: &str,
) -> Result<Value, ConfigError> {
    let path = format!("/api/conversations/{}", encode_path_segment(&env.conversation_id));
    request_json(client, env, Method::GET, &path, None, command).await
}

async fn resolve_current_assistant_id(
    client: &reqwest::Client,
    env: &ConfigEnv,
    command: &str,
) -> Result<String, ConfigError> {
    let conversation = fetch_current_conversation(client, env, command).await?;
    conversation
        .get("assistant")
        .and_then(|assistant| assistant.get("id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| {
            ConfigError::new(
                ConfigErrorCode::ContextAssistantMissing,
                command,
                "current conversation has no assistant",
            )
        })
}

async fn resolve_top_level_selectors(
    client: &reqwest::Client,
    env: &ConfigEnv,
    command: &str,
    payload: &mut Value,
    selectors: &mut SelectorMeta,
) -> Result<(), ConfigError> {
    let object = payload.as_object_mut().ok_or_else(|| {
        ConfigError::new(ConfigErrorCode::PayloadInvalid, command, "JSON 载荷必须是对象").field("field", "stdin")
    })?;

    if is_current_selector(object.get("conversation_id")) {
        object.insert("conversation_id".into(), Value::String(env.conversation_id.clone()));
        selectors.insert("conversation_id", env.conversation_id.clone());
    }
    if is_current_selector(object.get("user_id")) {
        object.insert("user_id".into(), Value::String(env.user_id.clone()));
        selectors.insert("user_id", env.user_id.clone());
    }
    if is_current_selector(object.get("assistant_id")) {
        let assistant_id = resolve_current_assistant_id(client, env, command).await?;
        object.insert("assistant_id".into(), Value::String(assistant_id.clone()));
        selectors.insert("assistant_id", assistant_id);
    }

    Ok(())
}

fn is_current_selector(value: Option<&Value>) -> bool {
    value.and_then(Value::as_str) == Some("current")
}

#[derive(Default)]
struct SelectorMeta {
    resolved: Map<String, Value>,
}

impl SelectorMeta {
    fn insert(&mut self, field: &'static str, value: impl Into<String>) {
        self.resolved.insert(field.into(), Value::String(value.into()));
    }

    fn into_map(self) -> Map<String, Value> {
        let mut map = Map::new();
        if !self.resolved.is_empty() {
            map.insert("resolved_selectors".into(), Value::Object(self.resolved));
        }
        map
    }
}

fn meta(selectors: Option<SelectorMeta>) -> Value {
    match selectors {
        Some(selectors) => meta_from_map(selectors.into_map()),
        None => meta_from_map(Map::new()),
    }
}

fn meta_from_map(extra: Map<String, Value>) -> Value {
    let mut map = Map::new();
    map.insert("schema_version".into(), Value::Number(1.into()));
    for (key, value) in extra {
        map.insert(key, value);
    }
    Value::Object(map)
}

async fn request_json(
    client: &reqwest::Client,
    env: &ConfigEnv,
    method: Method,
    path: &str,
    body: Option<Value>,
    command: &str,
) -> Result<Value, ConfigError> {
    let url = format!("{}{}", env.base_url, path);
    let method_label = method.as_str().to_owned();
    let mut request = client
        .request(method, &url)
        .header("content-type", "application/json")
        .header("x-tjuaeui-conversation-id", &env.conversation_id)
        .header("x-tjuaeui-user-id", &env.user_id);
    if let Some(body) = body {
        request = request.json(&body);
    }

    let response = request.send().await.map_err(|_| {
        tracing::warn!(
            command,
            method = method_label.as_str(),
            path,
            "config CLI backend request failed"
        );
        ConfigError::new(
            ConfigErrorCode::HttpRequestFailed,
            command,
            "failed to call TjuaeUI backend",
        )
        .field("path", path)
    })?;

    let status = response.status();
    let text = response.text().await.map_err(|_| {
        ConfigError::new(
            ConfigErrorCode::ResponseReadFailed,
            command,
            "failed to read TjuaeUI backend response",
        )
        .field("path", path)
    })?;

    if !status.is_success() {
        tracing::warn!(
            command,
            method = method_label.as_str(),
            path,
            status = status.as_u16(),
            "config CLI backend returned error status"
        );
        return Err(
            ConfigError::new(ConfigErrorCode::HttpStatusError, command, "TjuaeUI 后端返回错误状态")
                .field("path", path)
                .field("status", status.as_u16().to_string()),
        );
    }

    if text.trim().is_empty() {
        return Ok(Value::Null);
    }

    let value: Value = serde_json::from_str(&text).map_err(|_| {
        tracing::warn!(
            command,
            method = method_label.as_str(),
            path,
            status = status.as_u16(),
            "config CLI backend returned invalid JSON"
        );
        ConfigError::new(
            ConfigErrorCode::ResponseJsonInvalid,
            command,
            "TjuaeUI 后端返回无效 JSON",
        )
        .field("path", path)
    })?;
    if method_label == "GET" {
        tracing::debug!(
            command,
            method = method_label.as_str(),
            path,
            status = status.as_u16(),
            "config CLI backend read succeeded"
        );
    } else {
        tracing::info!(
            command,
            method = method_label.as_str(),
            path,
            status = status.as_u16(),
            "config CLI backend write/probe succeeded"
        );
    }
    extract_api_data(value, command)
}

fn extract_api_data(value: Value, command: &str) -> Result<Value, ConfigError> {
    let Some(success) = value.get("success").and_then(Value::as_bool) else {
        return Ok(value);
    };

    if success {
        return Ok(value.get("data").cloned().unwrap_or(Value::Null));
    }

    Err(ConfigError::new(
        ConfigErrorCode::HttpStatusError,
        command,
        "TjuaeUI 后端返回失败响应",
    ))
}

fn read_stdin_payload(command: &str) -> Result<Value, ConfigError> {
    let mut raw = String::new();
    io::stdin().read_to_string(&mut raw).map_err(|_| {
        ConfigError::new(
            ConfigErrorCode::PayloadInvalid,
            command,
            "failed to read JSON payload from stdin",
        )
        .field("field", "stdin")
    })?;
    if raw.trim().is_empty() {
        return Err(ConfigError::new(
            ConfigErrorCode::PayloadMissing,
            command,
            "必须通过标准输入提供 JSON 载荷",
        )
        .field("field", "stdin"));
    }
    serde_json::from_str(&raw).map_err(|_| {
        ConfigError::new(
            ConfigErrorCode::PayloadInvalid,
            command,
            "invalid JSON payload on stdin",
        )
        .field("field", "stdin")
    })
}

fn print_envelope(data: Value, meta: Value, command: &str) -> Result<(), ConfigError> {
    let rendered = serde_json::to_string_pretty(&json!({
        "success": true,
        "data": data,
        "meta": meta,
    }))
    .map_err(|_| {
        ConfigError::new(
            ConfigErrorCode::StdoutWriteFailed,
            command,
            "failed to serialize JSON output",
        )
    })?;
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(rendered.as_bytes())
        .and_then(|_| stdout.write_all(b"\n"))
        .map_err(|_| {
            ConfigError::new(
                ConfigErrorCode::StdoutWriteFailed,
                command,
                "failed to write JSON output",
            )
        })?;
    Ok(())
}

fn required_string_field(payload: &Value, field: &'static str, command: &str) -> Result<String, ConfigError> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            ConfigError::new(ConfigErrorCode::PayloadInvalid, command, "missing required field").field("field", field)
        })
}

fn take_required_string_field(payload: &mut Value, field: &'static str, command: &str) -> Result<String, ConfigError> {
    let object = payload.as_object_mut().ok_or_else(|| {
        ConfigError::new(ConfigErrorCode::PayloadInvalid, command, "JSON 载荷必须是对象").field("field", "stdin")
    })?;
    object
        .remove(field)
        .and_then(|value| value.as_str().map(str::to_owned))
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            ConfigError::new(ConfigErrorCode::PayloadInvalid, command, "missing required field").field("field", field)
        })
}

fn take_optional_string_field(payload: &mut Value, field: &'static str) -> Option<String> {
    payload
        .as_object_mut()
        .and_then(|object| object.remove(field))
        .and_then(|value| value.as_str().map(str::to_owned))
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn take_required_bool_field(
    object: &mut Map<String, Value>,
    field: &'static str,
    command: &str,
) -> Result<bool, ConfigError> {
    object.remove(field).and_then(|value| value.as_bool()).ok_or_else(|| {
        ConfigError::new(
            ConfigErrorCode::PayloadInvalid,
            command,
            "missing or invalid required field",
        )
        .field("field", field)
    })
}

fn reject_remaining_fields(payload: &Value, command: &str) -> Result<(), ConfigError> {
    let object = payload.as_object().ok_or_else(|| {
        ConfigError::new(ConfigErrorCode::PayloadInvalid, command, "JSON 载荷必须是对象").field("field", "stdin")
    })?;
    if let Some(field) = object.keys().next() {
        return Err(
            ConfigError::new(ConfigErrorCode::PayloadInvalid, command, "unknown field").field("field", field.clone()),
        );
    }
    Ok(())
}

fn redacted_content_summary(value: Value) -> Value {
    match value {
        Value::String(content) => json!({
            "redacted": true,
            "chars": content.chars().count(),
        }),
        other => redact_meta_value(other),
    }
}

fn redact_meta_value(value: Value) -> Value {
    match value {
        Value::Object(object) => {
            let redacted = object
                .into_iter()
                .map(|(key, value)| {
                    let value = if should_redact_meta_key(&key) {
                        redacted_content_summary(value)
                    } else {
                        redact_meta_value(value)
                    };
                    (key, value)
                })
                .collect();
            Value::Object(redacted)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(redact_meta_value).collect()),
        other => other,
    }
}

fn should_redact_meta_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    matches!(
        key.as_str(),
        "api_key"
            | "access_key"
            | "secret_key"
            | "aws_access_key_id"
            | "aws_secret_access_key"
            | "authorization"
            | "headers"
            | "env"
            | "rules"
            | "content"
            | "prompt"
            | "prompts"
            | "prompts_i18n"
            | "recommended"
            | "recommended_i18n"
    ) || key.contains("secret")
        || key.contains("token")
        || key.contains("password")
}

fn encode_path_segment(input: &str) -> String {
    percent_encode(input, false)
}

fn encode_query_component(input: &str) -> String {
    percent_encode(input, true)
}

fn percent_encode(input: &str, encode_space_as_plus: bool) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            b' ' if encode_space_as_plus => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigErrorCode {
    EnvMissing,
    ContextAssistantMissing,
    PayloadMissing,
    PayloadInvalid,
    HttpRequestFailed,
    HttpStatusError,
    ResponseReadFailed,
    ResponseJsonInvalid,
    StdoutWriteFailed,
}

impl ConfigErrorCode {
    fn as_str(self) -> &'static str {
        match self {
            Self::EnvMissing => "CONFIG_ENV_MISSING",
            Self::ContextAssistantMissing => "CONFIG_CONTEXT_ASSISTANT_MISSING",
            Self::PayloadMissing => "CONFIG_PAYLOAD_MISSING",
            Self::PayloadInvalid => "CONFIG_PAYLOAD_INVALID",
            Self::HttpRequestFailed => "CONFIG_HTTP_REQUEST_FAILED",
            Self::HttpStatusError => "CONFIG_HTTP_STATUS_ERROR",
            Self::ResponseReadFailed => "CONFIG_RESPONSE_READ_FAILED",
            Self::ResponseJsonInvalid => "CONFIG_RESPONSE_JSON_INVALID",
            Self::StdoutWriteFailed => "CONFIG_STDOUT_WRITE_FAILED",
        }
    }

    fn exit_code(self) -> ExitCode {
        match self {
            Self::EnvMissing | Self::ContextAssistantMissing | Self::PayloadMissing | Self::PayloadInvalid => {
                ExitCode::from(2)
            }
            Self::HttpRequestFailed | Self::HttpStatusError => ExitCode::from(3),
            Self::ResponseReadFailed | Self::ResponseJsonInvalid | Self::StdoutWriteFailed => ExitCode::from(1),
        }
    }
}

#[derive(Debug)]
struct ConfigError {
    code: ConfigErrorCode,
    command: String,
    message: &'static str,
    fields: BTreeMap<&'static str, String>,
}

impl ConfigError {
    fn new(code: ConfigErrorCode, command: &str, message: &'static str) -> Self {
        Self {
            code,
            command: command.to_owned(),
            message,
            fields: BTreeMap::new(),
        }
    }

    fn field(mut self, key: &'static str, value: impl Into<String>) -> Self {
        self.fields.insert(key, value.into());
        self
    }

    fn exit_code(&self) -> ExitCode {
        self.code.exit_code()
    }

    fn stderr_line(&self) -> String {
        let mut line = format!(
            "{} command=\"{}\"",
            self.code.as_str(),
            escape_stderr_field(&self.command)
        );
        for (key, value) in &self.fields {
            line.push_str(&format!(" {key}=\"{}\"", escape_stderr_field(value)));
        }
        line.push_str(": ");
        line.push_str(self.message);
        line
    }

    fn log_failure(&self) {
        tracing::warn!(
            code = self.code.as_str(),
            command = self.command.as_str(),
            "config CLI command failed"
        );
    }
}

fn escape_stderr_field(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_error_renders_quoted_stable_contract() {
        let error = ConfigError::new(
            ConfigErrorCode::EnvMissing,
            "config context",
            "missing required environment variable",
        )
        .field("field", ENV_CONVERSATION_ID);

        assert_eq!(
            error.stderr_line(),
            "CONFIG_ENV_MISSING command=\"config context\" field=\"TJUAE_CONVERSATION_ID\": missing required environment variable"
        );
    }

    #[test]
    fn path_segments_are_percent_encoded() {
        assert_eq!(encode_path_segment("a/b c"), "a%2Fb%20c");
    }
}
