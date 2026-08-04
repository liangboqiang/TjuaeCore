//! Agent 能力目录在运行时表示与持久化握手快照之间的转换。
//!
//! 会话建立和主动诊断必须复用同一套投影，避免模型列表在两条路径上
//! 产生不同的 JSON 结构。

use tjuaeui_api_types::AgentHandshake;
use tjuaeui_session::Capabilities;

/// 将 session backend 发现的模型、模式和斜杠命令投影为持久化目录快照。
pub(crate) fn handshake_from_session_capabilities(caps: &Capabilities) -> Option<AgentHandshake> {
    let mut config_options = Vec::new();
    if !caps.available_modes.is_empty() {
        config_options.push(serde_json::json!({
            "id": "mode",
            "category": "mode",
            "type": "select",
            "currentValue": caps.current_mode,
            "options": caps.available_modes.iter().map(|mode| serde_json::json!({
                "value": mode.id,
                "name": mode.name,
                "description": mode.description,
            })).collect::<Vec<_>>(),
        }));
    }
    if !caps.available_models.is_empty() {
        config_options.push(serde_json::json!({
            "id": "model",
            "category": "model",
            "type": "select",
            "currentValue": caps.current_model,
            "options": caps.available_models.iter().map(|model| serde_json::json!({
                "value": model.id,
                "name": model.name,
                "description": model.description,
            })).collect::<Vec<_>>(),
        }));
    }

    let available_commands = (!caps.slash_commands.is_empty()).then(|| {
        serde_json::json!(
            caps.slash_commands
                .iter()
                .map(|command| serde_json::json!({
                    "name": command.name,
                    "description": command.description,
                }))
                .collect::<Vec<_>>()
        )
    });
    if config_options.is_empty() && available_commands.is_none() {
        return None;
    }

    let config_options = (!config_options.is_empty()).then_some(serde_json::Value::Array(config_options));
    let available_modes = (!caps.available_modes.is_empty()).then(|| {
        serde_json::json!({
            "available_modes": caps.available_modes.iter().map(|mode| serde_json::json!({
                "id": mode.id,
                "name": mode.name,
                "description": mode.description,
            })).collect::<Vec<_>>(),
            "current_mode_id": caps.current_mode,
        })
    });
    let available_models = (!caps.available_models.is_empty()).then(|| {
        serde_json::json!({
            "available_models": caps.available_models.iter().map(|model| serde_json::json!({
                "id": model.id,
                "label": model.name,
            })).collect::<Vec<_>>(),
            "current_model_id": caps.current_model,
        })
    });

    Some(AgentHandshake {
        config_options,
        available_modes,
        available_models,
        available_commands,
        ..Default::default()
    })
}
