use crate::constants::RESERVED_NAME_PREFIXES;
use crate::error::ExtensionError;
use crate::types::ExtensionManifest;
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Validate an extension manifest for required fields, name format, and version format.
pub fn validate_manifest(manifest: &ExtensionManifest) -> Result<(), ExtensionError> {
    validate_name(&manifest.name)?;
    validate_version(&manifest.version)?;
    Ok(())
}

/// Reject extension names that use reserved prefixes.
fn validate_name(name: &str) -> Result<(), ExtensionError> {
    if name.is_empty() {
        return Err(ExtensionError::ManifestValidation(
            "extension name must not be empty".into(),
        ));
    }

    let lower = name.to_lowercase();
    for prefix in RESERVED_NAME_PREFIXES {
        if lower.starts_with(prefix) {
            return Err(ExtensionError::ReservedNamePrefix {
                name: name.to_owned(),
                prefix: (*prefix).to_owned(),
            });
        }
    }
    Ok(())
}

/// Validate that the version string is valid semver.
fn validate_version(version: &str) -> Result<(), ExtensionError> {
    if version.is_empty() {
        return Err(ExtensionError::ManifestValidation(
            "extension version must not be empty".into(),
        ));
    }

    semver::Version::parse(version).map_err(|e| ExtensionError::InvalidVersion {
        version: version.to_owned(),
        reason: e.to_string(),
    })?;
    Ok(())
}

/// Parse and validate a manifest from JSON bytes.
pub fn parse_manifest(json_bytes: &[u8]) -> Result<ExtensionManifest, ExtensionError> {
    parse_manifest_inner(json_bytes, None)
}

/// Parse and validate a manifest from JSON bytes, resolving `$file:`
/// references relative to the extension directory before deserialization.
pub fn parse_manifest_in_dir(json_bytes: &[u8], extension_dir: &Path) -> Result<ExtensionManifest, ExtensionError> {
    parse_manifest_inner(json_bytes, Some(extension_dir))
}

fn parse_manifest_inner(json_bytes: &[u8], extension_dir: Option<&Path>) -> Result<ExtensionManifest, ExtensionError> {
    let mut manifest_json: Value = serde_json::from_slice(json_bytes)?;
    if let Some(dir) = extension_dir {
        let mut visited = HashSet::new();
        manifest_json = resolve_file_refs(manifest_json, dir, &mut visited)?;
    }
    normalize_manifest_json(&mut manifest_json);
    let manifest: ExtensionManifest = serde_json::from_value(manifest_json)?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

fn resolve_file_refs(
    value: Value,
    extension_dir: &Path,
    visited: &mut HashSet<PathBuf>,
) -> Result<Value, ExtensionError> {
    match value {
        Value::String(text) if is_file_ref(&text) => resolve_file_ref_value(&text, extension_dir, visited),
        Value::Array(values) => {
            let mut resolved = Vec::with_capacity(values.len());
            for item in values {
                resolved.push(resolve_file_refs(item, extension_dir, visited)?);
            }
            Ok(Value::Array(resolved))
        }
        Value::Object(map) => {
            let mut resolved = Map::with_capacity(map.len());
            for (key, value) in map {
                resolved.insert(key, resolve_file_refs(value, extension_dir, visited)?);
            }
            Ok(Value::Object(resolved))
        }
        other => Ok(other),
    }
}

fn is_file_ref(value: &str) -> bool {
    value.starts_with("$file:")
}

fn resolve_file_ref_value(
    reference: &str,
    extension_dir: &Path,
    visited: &mut HashSet<PathBuf>,
) -> Result<Value, ExtensionError> {
    let relative = reference.trim_start_matches("$file:").trim();
    let absolute = extension_dir.join(relative);
    let canonical_base = std::fs::canonicalize(extension_dir)?;
    let canonical_path =
        std::fs::canonicalize(&absolute).map_err(|_| ExtensionError::FileReferenceNotFound(relative.to_owned()))?;

    if !canonical_path.starts_with(&canonical_base) {
        return Err(ExtensionError::PathTraversal(relative.to_owned()));
    }

    if !visited.insert(canonical_path.clone()) {
        return Err(ExtensionError::ManifestValidation(format!(
            "circular $file reference detected: {relative}"
        )));
    }

    let content = std::fs::read_to_string(&canonical_path)?;
    let resolved = match canonical_path.extension().and_then(|ext| ext.to_str()) {
        Some("json") | Some("jsonc") | Some("json5") => {
            let parsed: Value = serde_json::from_str(&content)?;
            resolve_file_refs(parsed, extension_dir, visited)?
        }
        _ => Value::String(content.trim_end_matches('\n').to_owned()),
    };

    visited.remove(&canonical_path);
    Ok(resolved)
}

fn normalize_manifest_json(value: &mut Value) {
    let Some(root) = value.as_object_mut() else {
        return;
    };

    move_key(root, "displayName", "display_name");
    move_key(root, "apiVersion", "api_version");
    move_key(root, "entryPoint", "entry_point");

    if let Some(i18n) = root.get_mut("i18n") {
        normalize_i18n(i18n);
    }

    if let Some(contributes) = root.get_mut("contributes") {
        normalize_contributes(contributes);
    }
}

fn normalize_i18n(value: &mut Value) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };

    move_key(obj, "localesDir", "directory");
    if !obj.contains_key("locales") {
        if let Some(default_locale) = obj.remove("defaultLocale") {
            obj.insert("locales".into(), Value::Array(vec![default_locale]));
        }
    } else {
        obj.remove("defaultLocale");
    }
}

fn normalize_contributes(value: &mut Value) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };

    move_key(obj, "channelPlugins", "channel_plugins");
    move_key(obj, "settingsTabs", "settings_tabs");
    move_key(obj, "modelProviders", "model_providers");

    normalize_array_entries(obj.get_mut("channel_plugins"), normalize_channel_plugin);
    normalize_array_entries(obj.get_mut("themes"), normalize_theme);
    normalize_array_entries(obj.get_mut("model_providers"), normalize_model_provider);
}

fn normalize_array_entries(value: Option<&mut Value>, normalize_item: fn(&mut Value)) {
    let Some(Value::Array(items)) = value else {
        return;
    };

    for item in items {
        normalize_item(item);
    }
}

fn normalize_channel_plugin(value: &mut Value) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };

    if !obj.contains_key("id") {
        move_key(obj, "type", "id");
    }
    move_key(obj, "entryPoint", "entry_point");
}

fn normalize_theme(value: &mut Value) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };

    move_key(obj, "file", "css_file");
    move_key(obj, "cover", "cover_image");
}

fn normalize_model_provider(value: &mut Value) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };

    move_key(obj, "baseUrl", "base_url");
    move_key(obj, "platform", "protocol");
}

fn move_key(map: &mut Map<String, Value>, old_key: &str, new_key: &str) {
    if map.contains_key(new_key) {
        map.remove(old_key);
        return;
    }

    if let Some(value) = map.remove(old_key) {
        map.insert(new_key.to_owned(), value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    // -- validate_manifest --

    #[test]
    fn test_valid_manifest() {
        let manifest = ExtensionManifest {
            name: "my-cool-ext".into(),
            version: "1.0.0".into(),
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

    #[test]
    fn test_empty_name_rejected() {
        let manifest = ExtensionManifest {
            name: "".into(),
            version: "1.0.0".into(),
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
        let err = validate_manifest(&manifest).unwrap_err();
        assert!(matches!(err, ExtensionError::ManifestValidation(_)));
    }

    #[test]
    fn test_reserved_prefix_tjuae() {
        let manifest = ExtensionManifest {
            name: "tjuae-my-ext".into(),
            version: "1.0.0".into(),
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
        let err = validate_manifest(&manifest).unwrap_err();
        assert!(matches!(
            err,
            ExtensionError::ReservedNamePrefix { ref prefix, .. } if prefix == "tjuae-"
        ));
    }

    #[test]
    fn test_all_reserved_prefixes_rejected() {
        for prefix in RESERVED_NAME_PREFIXES {
            let name = format!("{prefix}test");
            let manifest = ExtensionManifest {
                name,
                version: "1.0.0".into(),
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
            assert!(
                validate_manifest(&manifest).is_err(),
                "prefix '{prefix}' should be rejected"
            );
        }
    }

    #[test]
    fn test_reserved_prefix_case_insensitive() {
        let manifest = ExtensionManifest {
            name: "TJUAE-upper".into(),
            version: "1.0.0".into(),
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
        assert!(validate_manifest(&manifest).is_err());
    }

    #[test]
    fn test_empty_version_rejected() {
        let manifest = ExtensionManifest {
            name: "my-ext".into(),
            version: "".into(),
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
        let err = validate_manifest(&manifest).unwrap_err();
        assert!(matches!(err, ExtensionError::ManifestValidation(_)));
    }

    #[test]
    fn test_invalid_semver_rejected() {
        let manifest = ExtensionManifest {
            name: "my-ext".into(),
            version: "not-semver".into(),
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
        let err = validate_manifest(&manifest).unwrap_err();
        assert!(matches!(err, ExtensionError::InvalidVersion { .. }));
    }

    #[test]
    fn test_valid_semver_versions() {
        for version in &["0.0.1", "1.0.0", "1.2.3", "10.20.30", "1.0.0-alpha.1"] {
            let manifest = ExtensionManifest {
                name: "ext".into(),
                version: (*version).into(),
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
            assert!(
                validate_manifest(&manifest).is_ok(),
                "version '{version}' should be accepted"
            );
        }
    }

    // -- parse_manifest --

    #[test]
    fn test_parse_manifest_valid() {
        let raw = json!({"name": "my-ext", "version": "1.0.0"});
        let bytes = serde_json::to_vec(&raw).unwrap();
        let manifest = parse_manifest(&bytes).unwrap();
        assert_eq!(manifest.name, "my-ext");
        assert_eq!(manifest.version, "1.0.0");
    }

    #[test]
    fn test_parse_manifest_invalid_json() {
        let err = parse_manifest(b"not json").unwrap_err();
        assert!(matches!(err, ExtensionError::JsonParse(_)));
    }

    #[test]
    fn test_parse_manifest_missing_name() {
        let raw = json!({"version": "1.0.0"});
        let bytes = serde_json::to_vec(&raw).unwrap();
        let err = parse_manifest(&bytes).unwrap_err();
        assert!(matches!(err, ExtensionError::JsonParse(_)));
    }

    #[test]
    fn test_parse_manifest_reserved_name() {
        let raw = json!({"name": "internal-test", "version": "1.0.0"});
        let bytes = serde_json::to_vec(&raw).unwrap();
        let err = parse_manifest(&bytes).unwrap_err();
        assert!(matches!(err, ExtensionError::ReservedNamePrefix { .. }));
    }

    #[test]
    fn test_parse_manifest_in_dir_supports_tjuaeui_main_contract() {
        let tmp = TempDir::new().unwrap();
        let contributes_dir = tmp.path().join("contributes");
        std::fs::create_dir_all(&contributes_dir).unwrap();
        std::fs::write(
            contributes_dir.join("settings-tabs.json"),
            serde_json::to_vec(&json!([
                {
                    "id": "extension-settings",
                    "label": "Extension Settings",
                    "url": "settings/index.html",
                    "position": { "relativeTo": "display", "placement": "after" }
                }
            ]))
            .unwrap(),
        )
        .unwrap();

        let raw = json!({
            "name": "main-contract-ext",
            "displayName": "Main Contract Extension",
            "version": "1.0.0",
            "i18n": {
                "localesDir": "i18n",
                "defaultLocale": "en-US"
            },
            "contributes": {
                "settingsTabs": "$file:contributes/settings-tabs.json"
            }
        });

        let manifest = parse_manifest_in_dir(&serde_json::to_vec(&raw).unwrap(), tmp.path()).unwrap();
        assert_eq!(manifest.display_name.as_deref(), Some("Main Contract Extension"));
        assert_eq!(manifest.i18n.as_ref().unwrap().locales, vec!["en-US".to_owned()]);
        assert_eq!(manifest.i18n.as_ref().unwrap().directory, "i18n");
        let settings_tabs = &manifest.contributes.as_ref().unwrap().settings_tabs;
        assert_eq!(settings_tabs.len(), 1);
        assert_eq!(settings_tabs[0].label, "Extension Settings");
        assert_eq!(settings_tabs[0].url, "settings/index.html");
        assert_eq!(settings_tabs[0].position.as_ref().unwrap().relative_to, "display");
    }
}
