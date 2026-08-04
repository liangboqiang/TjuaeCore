//! Integration tests for contribution resolution (test-plan CR-1..CR-10).
//!
//! These are black-box tests that exercise the public resolver APIs with
//! realistic extension manifests and file system fixtures.

use std::collections::HashMap;

use tjuaeui_extension::types::*;
use tjuaeui_extension::{resolve_all_contributions, resolve_extension_contributions, resolve_i18n_for_all};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_loaded_extension(name: &str, dir: &str, contributes: ExtContributes) -> LoadedExtension {
    LoadedExtension {
        manifest: ExtensionManifest {
            name: name.to_owned(),
            version: "1.0.0".to_owned(),
            display_name: None,
            description: None,
            author: None,
            license: None,
            homepage: None,
            icon: None,
            engine: None,
            api_version: None,
            dependencies: HashMap::new(),
            entry_point: None,
            permissions: None,
            contributes: Some(contributes),
            i18n: None,
        },
        directory: dir.to_owned(),
        source: ExtensionSource::Local,
        state: ExtensionState {
            name: name.to_owned(),
            version: "1.0.0".to_owned(),
            enabled: true,
            installed_at: None,
            last_activated_at: None,
        },
    }
}

fn make_loaded_extension_with_i18n(name: &str, dir: &str, i18n: I18nConfig) -> LoadedExtension {
    LoadedExtension {
        manifest: ExtensionManifest {
            name: name.to_owned(),
            version: "1.0.0".to_owned(),
            display_name: None,
            description: None,
            author: None,
            license: None,
            homepage: None,
            icon: None,
            engine: None,
            api_version: None,
            dependencies: HashMap::new(),
            entry_point: None,
            permissions: None,
            contributes: None,
            i18n: Some(i18n),
        },
        directory: dir.to_owned(),
        source: ExtensionSource::Local,
        state: ExtensionState {
            name: name.to_owned(),
            version: "1.0.0".to_owned(),
            enabled: true,
            installed_at: None,
            last_activated_at: None,
        },
    }
}

// ---------------------------------------------------------------------------
// CR-6: Theme resolution (CSS content loaded)
// ---------------------------------------------------------------------------

#[test]
fn cr6_theme_css_content_loaded() {
    let dir = std::env::temp_dir().join("cr6_theme_resolve");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("dark.css"), ":root { --bg: #1a1a2e; --text: #eaeaea; }").unwrap();

    let contributes = ExtContributes {
        themes: vec![ExtTheme {
            id: "dark-theme".into(),
            name: "Dark Theme".into(),
            description: Some("A dark color scheme".into()),
            css_file: "dark.css".into(),
            cover_image: Some("images/dark-preview.png".into()),
        }],
        ..Default::default()
    };

    let ext = make_loaded_extension("theme-ext", &dir.to_string_lossy(), contributes);
    let result = resolve_extension_contributions(&ext);

    assert_eq!(result.themes.len(), 1);
    let theme = &result.themes[0];
    assert_eq!(theme.extension_name, "theme-ext");
    assert_eq!(theme.css_content, ":root { --bg: #1a1a2e; --text: #eaeaea; }");
    assert!(theme.cover_image.as_ref().unwrap().contains("images/dark-preview.png"));

    std::fs::remove_dir_all(&dir).unwrap();
}

// ---------------------------------------------------------------------------
// CR-7: WebUI route namespace validation
// ---------------------------------------------------------------------------

#[test]
fn cr7_webui_valid_namespace_resolves() {
    let contributes = ExtContributes {
        webui: vec![ExtWebui {
            id: "dashboard".into(),
            directory: "dist".into(),
            routes: vec![ExtWebuiRoute {
                path: "/my-ext/dashboard".into(),
                method: "GET".into(),
                handler: "handler.js".into(),
            }],
        }],
        ..Default::default()
    };

    let ext = make_loaded_extension("my-ext", "/ext/my-ext", contributes);
    let result = resolve_extension_contributions(&ext);

    assert_eq!(result.webui.len(), 1);
    assert_eq!(result.webui[0].extension_name, "my-ext");
}

#[test]
fn cr7_webui_wrong_namespace_rejected() {
    let contributes = ExtContributes {
        webui: vec![ExtWebui {
            id: "bad-route".into(),
            directory: "dist".into(),
            routes: vec![ExtWebuiRoute {
                path: "/other-ext/api".into(),
                method: "GET".into(),
                handler: "handler.js".into(),
            }],
        }],
        ..Default::default()
    };

    let ext = make_loaded_extension("my-ext", "/ext/my-ext", contributes);
    let result = resolve_extension_contributions(&ext);

    // Invalid route should be filtered out
    assert!(result.webui.is_empty());
}

#[test]
fn cr7_webui_reserved_prefix_rejected() {
    let contributes = ExtContributes {
        webui: vec![ExtWebui {
            id: "reserved".into(),
            directory: "dist".into(),
            routes: vec![ExtWebuiRoute {
                path: "/api/data".into(),
                method: "GET".into(),
                handler: "handler.js".into(),
            }],
        }],
        ..Default::default()
    };

    // Extension name "api" would match namespace but /api/ is reserved
    let ext = make_loaded_extension("api", "/ext/api", contributes);
    let result = resolve_extension_contributions(&ext);
    assert!(result.webui.is_empty());
}

// ---------------------------------------------------------------------------
// CR-8: Settings tab with position
// ---------------------------------------------------------------------------

#[test]
fn cr8_settings_tab_position_preserved() {
    let contributes = ExtContributes {
        settings_tabs: vec![ExtSettingsTab {
            id: "ext-settings".into(),
            label: "My Extension".into(),
            icon: Some("icons/gear.svg".into()),
            url: "settings/index.html".into(),
            position: Some(SettingsTabPosition {
                relative_to: "general".into(),
                placement: "after".into(),
            }),
            order: 80,
        }],
        ..Default::default()
    };

    let ext = make_loaded_extension("my-ext", "/ext/my-ext", contributes);
    let result = resolve_extension_contributions(&ext);

    assert_eq!(result.settings_tabs.len(), 1);
    let tab = &result.settings_tabs[0];
    assert_eq!(tab.extension_name, "my-ext");
    assert_eq!(tab.id, "ext-my-ext-ext-settings");
    assert_eq!(tab.url, "/api/extensions/my-ext/assets/settings/index.html");
    assert_eq!(
        tab.icon.as_deref(),
        Some("/api/extensions/my-ext/assets/icons/gear.svg")
    );
    assert_eq!(tab.order, 80);
    let pos = tab.position.as_ref().unwrap();
    assert_eq!(pos.relative_to, "general");
    assert_eq!(pos.placement, "after");
}

// ---------------------------------------------------------------------------
// CR-9: Model provider resolution
// ---------------------------------------------------------------------------

#[test]
fn cr9_model_provider_resolved() {
    let contributes = ExtContributes {
        model_providers: vec![ExtModelProvider {
            id: "custom-provider".into(),
            name: "Custom LLM".into(),
            description: Some("Custom model provider".into()),
            protocol: Some("openai".into()),
            base_url: Some("https://api.custom.com/v1".into()),
            models: vec!["custom-model-1".into(), "custom-model-2".into()],
        }],
        ..Default::default()
    };

    let ext = make_loaded_extension("provider-ext", "/ext/provider-ext", contributes);
    let result = resolve_extension_contributions(&ext);

    assert_eq!(result.model_providers.len(), 1);
    let provider = &result.model_providers[0];
    assert_eq!(provider.extension_name, "provider-ext");
    assert_eq!(provider.id, "custom-provider");
    assert_eq!(provider.protocol.as_deref(), Some("openai"));
    assert_eq!(provider.models.len(), 2);
}

// ---------------------------------------------------------------------------
// CR-10: i18n data loading
// ---------------------------------------------------------------------------

#[test]
fn cr10_i18n_data_loaded_for_supported_locale() {
    let dir = std::env::temp_dir().join("cr10_i18n_resolve");
    let i18n_dir = dir.join("i18n");
    std::fs::create_dir_all(&i18n_dir).unwrap();
    std::fs::write(
        i18n_dir.join("zh-CN.json"),
        r#"{"greeting": "你好", "settings.title": "设置"}"#,
    )
    .unwrap();

    let ext = make_loaded_extension_with_i18n(
        "i18n-ext",
        &dir.to_string_lossy(),
        I18nConfig {
            locales: vec!["en".into(), "zh-CN".into()],
            directory: "i18n".into(),
        },
    );

    let result = resolve_i18n_for_all(&[ext], "zh-CN");
    assert_eq!(result.len(), 1);
    let messages = &result["i18n-ext"];
    assert_eq!(messages["greeting"], "你好");
    assert_eq!(messages["settings.title"], "设置");

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn cr10_i18n_unsupported_locale_returns_empty() {
    let dir = std::env::temp_dir().join("cr10_i18n_unsupported");
    let i18n_dir = dir.join("i18n");
    std::fs::create_dir_all(&i18n_dir).unwrap();
    std::fs::write(i18n_dir.join("en.json"), r#"{"key": "value"}"#).unwrap();

    let ext = make_loaded_extension_with_i18n(
        "en-only-ext",
        &dir.to_string_lossy(),
        I18nConfig {
            locales: vec!["en".into()],
            directory: "i18n".into(),
        },
    );

    let result = resolve_i18n_for_all(&[ext], "fr");
    assert!(result.is_empty());

    std::fs::remove_dir_all(&dir).unwrap();
}

// ---------------------------------------------------------------------------
// Cross-cutting: resolve_all_contributions merges and filters
// ---------------------------------------------------------------------------

#[test]
fn resolve_all_merges_contributions_from_multiple_extensions() {
    let ext_a = make_loaded_extension(
        "ext-a",
        "/ext/a",
        ExtContributes {
            model_providers: vec![ExtModelProvider {
                id: "mp-a".into(),
                name: "Provider A".into(),
                description: None,
                protocol: None,
                base_url: None,
                models: vec![],
            }],
            ..Default::default()
        },
    );
    let ext_b = make_loaded_extension(
        "ext-b",
        "/ext/b",
        ExtContributes {
            settings_tabs: vec![ExtSettingsTab {
                id: "settings-b".into(),
                label: "设置 B".into(),
                icon: None,
                url: "settings/index.html".into(),
                position: None,
                order: 100,
            }],
            ..Default::default()
        },
    );

    let result = resolve_all_contributions(&[ext_a, ext_b]);
    assert_eq!(result.model_providers.len(), 1);
    assert_eq!(result.settings_tabs.len(), 1);
}

#[test]
fn resolve_all_skips_disabled_extensions() {
    let mut ext = make_loaded_extension(
        "disabled-ext",
        "/ext/disabled",
        ExtContributes {
            model_providers: vec![ExtModelProvider {
                id: "hidden".into(),
                name: "隐藏提供商".into(),
                description: None,
                protocol: None,
                base_url: None,
                models: vec![],
            }],
            ..Default::default()
        },
    );
    ext.state.enabled = false;

    let result = resolve_all_contributions(&[ext]);
    assert!(result.model_providers.is_empty());
}

#[test]
fn channel_plugin_resolved_with_entry_point() {
    let contributes = ExtContributes {
        channel_plugins: vec![ExtChannelPlugin {
            id: "slack".into(),
            name: "Slack".into(),
            description: Some("Slack channel plugin".into()),
            platform: Some("slack".into()),
            entry_point: Some("plugins/slack.js".into()),
            icon: Some("icons/slack.png".into()),
            credential_fields: vec![serde_json::json!({ "key": "token" })],
            config_fields: vec![serde_json::json!({ "key": "channel" })],
        }],
        ..Default::default()
    };

    let ext = make_loaded_extension("channel-ext", "/ext/channel-ext", contributes);
    let result = resolve_extension_contributions(&ext);

    assert_eq!(result.channel_plugins.len(), 1);
    assert_eq!(result.channel_plugins[0].extension_name, "channel-ext");
    assert!(
        result.channel_plugins[0]
            .entry_point
            .as_ref()
            .unwrap()
            .contains("plugins/slack.js")
    );
    assert_eq!(result.channel_plugins[0].credential_fields.len(), 1);
    assert_eq!(result.channel_plugins[0].config_fields.len(), 1);
}
