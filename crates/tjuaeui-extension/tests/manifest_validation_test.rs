//! Integration tests for manifest validation (test-plan MV-1 through MV-4).
//!
//! These test the public API surface of `tjuaeui_extension::manifest`.

use tjuaeui_extension::{ExtensionError, ExtensionManifest, parse_manifest, validate_manifest};

// -- MV-1: valid manifest loads successfully --

#[test]
fn mv1_valid_manifest_parses_and_validates() {
    let json = serde_json::json!({
        "name": "my-cool-extension",
        "version": "1.0.0",
        "display_name": "My Cool Extension",
        "description": "A test extension",
        "contributes": {
            "settingsTabs": [{
                "id": "test-settings",
                "label": "Test settings",
                "url": "settings/index.html"
            }]
        }
    });
    let bytes = serde_json::to_vec(&json).unwrap();
    let manifest = parse_manifest(&bytes).unwrap();
    assert_eq!(manifest.name, "my-cool-extension");
    assert_eq!(manifest.version, "1.0.0");
    assert!(manifest.contributes.is_some());
}

// -- MV-2: reserved name prefix rejected --

#[test]
fn mv2_reserved_prefix_tjuae_rejected() {
    let json = serde_json::json!({"name": "tjuae-my-ext", "version": "1.0.0"});
    let bytes = serde_json::to_vec(&json).unwrap();
    let err = parse_manifest(&bytes).unwrap_err();
    assert!(matches!(err, ExtensionError::ReservedNamePrefix { ref prefix, .. } if prefix == "tjuae-"));
}

#[test]
fn mv2_reserved_prefix_internal_rejected() {
    let json = serde_json::json!({"name": "internal-utils", "version": "1.0.0"});
    let bytes = serde_json::to_vec(&json).unwrap();
    assert!(parse_manifest(&bytes).is_err());
}

#[test]
fn mv2_reserved_prefix_builtin_rejected() {
    let json = serde_json::json!({"name": "builtin-theme", "version": "1.0.0"});
    let bytes = serde_json::to_vec(&json).unwrap();
    assert!(parse_manifest(&bytes).is_err());
}

#[test]
fn mv2_reserved_prefix_system_rejected() {
    let json = serde_json::json!({"name": "system-core", "version": "1.0.0"});
    let bytes = serde_json::to_vec(&json).unwrap();
    assert!(parse_manifest(&bytes).is_err());
}

// -- MV-3: missing required fields --

#[test]
fn mv3_missing_version_rejected() {
    let json = serde_json::json!({"name": "my-ext"});
    let bytes = serde_json::to_vec(&json).unwrap();
    // serde will fail because `version` is a required field
    assert!(parse_manifest(&bytes).is_err());
}

#[test]
fn mv3_missing_name_rejected() {
    let json = serde_json::json!({"version": "1.0.0"});
    let bytes = serde_json::to_vec(&json).unwrap();
    assert!(parse_manifest(&bytes).is_err());
}

// -- MV-4: invalid version rejected --

#[test]
fn mv4_invalid_version_rejected() {
    let json = serde_json::json!({"name": "my-ext", "version": "not-semver"});
    let bytes = serde_json::to_vec(&json).unwrap();
    let err = parse_manifest(&bytes).unwrap_err();
    assert!(matches!(err, ExtensionError::InvalidVersion { .. }));
}

#[test]
fn mv4_partial_version_rejected() {
    let json = serde_json::json!({"name": "my-ext", "version": "1.0"});
    let bytes = serde_json::to_vec(&json).unwrap();
    assert!(parse_manifest(&bytes).is_err());
}

// -- Edge cases --

#[test]
fn manifest_with_all_optional_fields() {
    let json = serde_json::json!({
        "name": "full-ext",
        "version": "2.0.0",
        "display_name": "Full Extension",
        "description": "Has everything",
        "author": "Test",
        "license": "MIT",
        "homepage": "https://example.com",
        "icon": "icon.png",
        "engine": { "tjuae": "^1.0.0" },
        "api_version": "1.0.0",
        "dependencies": { "other-ext": "^1.0.0" },
        "entry_point": "main.js",
        "permissions": { "storage": true, "events": true },
        "contributes": {},
        "i18n": { "locales": ["en", "zh-CN"] }
    });
    let bytes = serde_json::to_vec(&json).unwrap();
    let manifest = parse_manifest(&bytes).unwrap();
    assert_eq!(manifest.display_name.as_deref(), Some("Full Extension"));
    assert!(manifest.engine.is_some());
    assert!(manifest.i18n.is_some());
}

#[test]
fn core_asset_contributions_and_shell_lifecycle_are_rejected() {
    for forbidden in [
        serde_json::json!({ "contributes": { "assistants": [] } }),
        serde_json::json!({ "contributes": { "acpAdapters": [] } }),
        serde_json::json!({ "contributes": { "skills": [] } }),
        serde_json::json!({ "contributes": { "mcpServers": [] } }),
        serde_json::json!({ "lifecycle": { "onInstall": "scripts/install.ps1" } }),
    ] {
        let mut manifest = serde_json::json!({
            "name": "application-extension",
            "version": "1.0.0"
        });
        manifest
            .as_object_mut()
            .unwrap()
            .extend(forbidden.as_object().unwrap().clone());
        assert!(
            parse_manifest(&serde_json::to_vec(&manifest).unwrap()).is_err(),
            "应用扩展清单必须拒绝 Core 资产贡献和 Shell 生命周期钩子：{manifest}"
        );
    }
}

#[test]
fn validate_manifest_directly() {
    let manifest = ExtensionManifest {
        name: "valid-ext".into(),
        version: "1.0.0-beta.1".into(),
        display_name: None,
        description: None,
        author: None,
        license: None,
        homepage: None,
        icon: None,
        engine: None,
        api_version: None,
        dependencies: Default::default(),
        entry_point: None,
        permissions: None,
        contributes: None,
        i18n: None,
    };
    assert!(validate_manifest(&manifest).is_ok());
}
