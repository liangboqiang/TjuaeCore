use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::OnceLock;

use async_trait::async_trait;
use regex::Regex;
use serde_json::json;
use sha2::{Digest, Sha256};
use tjuaeui_api_types::{
    CanonicalAssetFile, CanonicalAssetPackage, HubAssetKind, HubAssetPublishPreparation, HubAssetPublishRequest,
    HubAssetPublishResponse, HubAssetPublishWarningCode, HubPublishConnectionState, HubPublishConnectionStatus,
};

use crate::publish_error::AssetPublishError;
use crate::publish_provider::{HubPublishProvider, HubPublisher};

pub(crate) const ASSET_PACKAGE_SCHEMA_URL: &str =
    "https://raw.githubusercontent.com/liangboqiang/TjuaeHub/main/schemas/asset-package.v1.schema.json";
const MAX_CANONICAL_FILE_BYTES: usize = 1024 * 1024;
const MAX_CANONICAL_TOTAL_BYTES: usize = 10 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct AssetTextFile {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct LocalAssetMaterial {
    pub display_name: String,
    pub description: String,
    pub runtime_id: String,
    pub definition_file: String,
    pub dependencies: Vec<String>,
    pub files: Vec<AssetTextFile>,
    pub blocked_fields: Vec<String>,
}

#[async_trait]
pub trait HubAssetPort: Send + Sync {
    /// Export a Definition from the authenticated user's Core asset catalog.
    async fn export_catalog(
        &self,
        user_id: &str,
        asset_kind: HubAssetKind,
        asset_id: &str,
    ) -> Result<LocalAssetMaterial, AssetPublishError>;
}

#[derive(Default)]
pub struct DisabledHubAssetPort;

#[async_trait]
impl HubAssetPort for DisabledHubAssetPort {
    async fn export_catalog(
        &self,
        _user_id: &str,
        _asset_kind: HubAssetKind,
        _asset_id: &str,
    ) -> Result<LocalAssetMaterial, AssetPublishError> {
        Err(AssetPublishError::HubPublishPrerequisite(
            "本地资产仓库尚未完成初始化".into(),
        ))
    }
}

#[derive(Clone)]
pub struct HubAssetService {
    port: Arc<dyn HubAssetPort>,
    publisher: Option<HubPublisher>,
}

impl HubAssetService {
    pub fn new(port: Arc<dyn HubAssetPort>) -> Self {
        Self { port, publisher: None }
    }

    pub fn with_publish_provider(mut self, provider: Arc<dyn HubPublishProvider>) -> Self {
        self.publisher = Some(HubPublisher::new(provider));
        self
    }

    pub async fn publish_request(
        &self,
        user_id: &str,
        request: &HubAssetPublishRequest,
    ) -> Result<HubAssetPublishPreparation, AssetPublishError> {
        let canonical = self.canonicalize_for_publish(user_id, request).await?;
        let mut preparation = self
            .publisher
            .as_ref()
            .ok_or_else(|| AssetPublishError::HubPublishPrerequisite("GITHUB_APP_NOT_CONFIGURED".into()))?
            .publish_request(request, canonical.package)?;
        preparation.warning_codes = canonical.warning_codes;
        preparation.blocked_fields = canonical.blocked_fields;
        Ok(preparation)
    }

    pub async fn publish(
        &self,
        user_id: &str,
        request: &HubAssetPublishRequest,
    ) -> Result<HubAssetPublishResponse, AssetPublishError> {
        let package = self.package_for_publish(user_id, request).await?;
        self.publisher
            .as_ref()
            .ok_or_else(|| AssetPublishError::HubPublishPrerequisite("GITHUB_APP_NOT_CONFIGURED".into()))?
            .publish(user_id, request, package)
            .await
    }

    pub async fn publish_connection_status(
        &self,
        user_id: &str,
    ) -> Result<HubPublishConnectionStatus, AssetPublishError> {
        match &self.publisher {
            Some(publisher) => publisher.connection_status(user_id).await,
            None => Ok(HubPublishConnectionStatus {
                state: HubPublishConnectionState::NotConfigured,
                account: None,
                user_code: None,
                verification_uri: None,
                expires_at: None,
                poll_after_ms: None,
                reason_code: Some("GITHUB_APP_NOT_CONFIGURED".into()),
            }),
        }
    }

    pub async fn start_publish_authorization(
        &self,
        user_id: &str,
    ) -> Result<HubPublishConnectionStatus, AssetPublishError> {
        self.publisher
            .as_ref()
            .ok_or_else(|| AssetPublishError::HubPublishPrerequisite("GITHUB_APP_NOT_CONFIGURED".into()))?
            .start_authorization(user_id)
            .await
    }

    pub async fn poll_publish_authorization(
        &self,
        user_id: &str,
    ) -> Result<HubPublishConnectionStatus, AssetPublishError> {
        self.publisher
            .as_ref()
            .ok_or_else(|| AssetPublishError::HubPublishPrerequisite("GITHUB_APP_NOT_CONFIGURED".into()))?
            .poll_authorization(user_id)
            .await
    }

    pub async fn disconnect_publish_account(
        &self,
        user_id: &str,
    ) -> Result<HubPublishConnectionStatus, AssetPublishError> {
        self.publisher
            .as_ref()
            .ok_or_else(|| AssetPublishError::HubPublishPrerequisite("GITHUB_APP_NOT_CONFIGURED".into()))?
            .disconnect(user_id)
            .await
    }

    async fn package_for_publish(
        &self,
        user_id: &str,
        request: &HubAssetPublishRequest,
    ) -> Result<CanonicalAssetPackage, AssetPublishError> {
        Ok(self.canonicalize_for_publish(user_id, request).await?.package)
    }

    async fn canonicalize_for_publish(
        &self,
        user_id: &str,
        request: &HubAssetPublishRequest,
    ) -> Result<CanonicalizedAsset, AssetPublishError> {
        let material = self
            .port
            .export_catalog(user_id, request.asset_kind.clone(), &request.asset_id)
            .await?;
        canonicalize(
            request.asset_kind.clone(),
            &request.asset_id,
            &request.package_name,
            &request.version,
            PublicationMetadata {
                author: &request.author,
                license: &request.license,
                source_repository: &request.source_repository,
                tags: &request.tags,
                confirmed: request.metadata_confirmed,
            },
            material,
        )
    }
}

struct CanonicalizedAsset {
    package: CanonicalAssetPackage,
    warning_codes: Vec<HubAssetPublishWarningCode>,
    blocked_fields: Vec<String>,
}

#[derive(Clone, Copy)]
struct PublicationMetadata<'a> {
    author: &'a str,
    license: &'a str,
    source_repository: &'a str,
    tags: &'a [String],
    confirmed: bool,
}

fn canonicalize(
    asset_kind: HubAssetKind,
    asset_id: &str,
    package_name: &str,
    version: &str,
    metadata: PublicationMetadata<'_>,
    mut material: LocalAssetMaterial,
) -> Result<CanonicalizedAsset, AssetPublishError> {
    validate_asset_local_id(asset_id, "assetId")?;
    validate_package_name(package_name)?;
    semver::Version::parse(version).map_err(|_| AssetPublishError::InvalidVersion {
        version: version.into(),
        reason: "必须是完整 SemVer".into(),
    })?;
    validate_publication_metadata(
        metadata.author,
        metadata.license,
        metadata.source_repository,
        metadata.confirmed,
    )?;
    let author = metadata.author.trim();
    let license = metadata.license.trim();
    let source_repository = metadata.source_repository.trim();
    validate_asset_local_id(&material.runtime_id, "runtimeId")?;
    let expected_definition_file = definition_file_for_kind(&asset_kind);
    if material.definition_file != expected_definition_file {
        return Err(AssetPublishError::InvalidRequest(format!(
            "{} 资产的 definitionFile 必须是 {expected_definition_file}",
            asset_kind_name(&asset_kind)
        )));
    }
    validate_asset_text(&material.display_name, 1, 128, "displayName")?;
    validate_asset_text(&material.description, 1, 4096, "description")?;
    validate_dependencies(&material.dependencies)?;
    let mut tags = metadata.tags.to_vec();
    tags.sort();
    tags.dedup();
    for tag in &tags {
        validate_asset_text(tag, 1, 64, "tags[]")?;
    }
    let files = canonical_files(material.files)?;
    if !files.iter().any(|file| file.path == expected_definition_file) {
        return Err(AssetPublishError::AssetSanitization(format!(
            "规范包缺少固定 Definition 入口：{expected_definition_file}"
        )));
    }
    let manifest = json!({
        "$schema": ASSET_PACKAGE_SCHEMA_URL,
        "schemaVersion": 1,
        "name": package_name,
        "version": version,
        "displayName": material.display_name,
        "description": material.description,
        "author": author,
        "license": license,
        "compatibility": {
            "tjuae": "^1.0.0"
        },
        "source": {
            "repository": source_repository,
            "license": license
        },
        "tags": tags,
        "assets": [{
            "kind": asset_kind_name(&asset_kind),
            "id": asset_id,
            "runtimeId": material.runtime_id,
            "definitionFile": expected_definition_file,
            "dependencies": material.dependencies
        }]
    });
    validate_public_asset_file("asset-package.json", &serde_json::to_string(&manifest)?)?;

    material.blocked_fields.sort();
    material.blocked_fields.dedup();
    let warning_codes = (!material.blocked_fields.is_empty())
        .then_some(HubAssetPublishWarningCode::SensitiveFieldsRemoved)
        .into_iter()
        .collect();
    Ok(CanonicalizedAsset {
        package: CanonicalAssetPackage {
            package_name: package_name.to_owned(),
            manifest,
            files,
        },
        warning_codes,
        blocked_fields: material.blocked_fields,
    })
}

pub(crate) fn validate_publication_metadata(
    author: &str,
    license: &str,
    source_repository: &str,
    metadata_confirmed: bool,
) -> Result<(), AssetPublishError> {
    if !metadata_confirmed {
        return Err(AssetPublishError::InvalidRequest(
            "发布者必须明确确认作者与许可证信息".into(),
        ));
    }
    let author = author.trim();
    let license = license.trim();
    let source_repository = source_repository.trim();
    if author.is_empty() || author.chars().count() > 128 || author.chars().any(char::is_control) {
        return Err(AssetPublishError::InvalidRequest(
            "author 必须是 1 到 128 个不含控制字符的字符".into(),
        ));
    }
    if license.is_empty() || license.chars().count() > 128 || license.chars().any(char::is_control) {
        return Err(AssetPublishError::InvalidRequest(
            "license 必须是 1 到 128 个不含控制字符的字符".into(),
        ));
    }
    let source = url::Url::parse(source_repository)
        .map_err(|_| AssetPublishError::InvalidRequest("sourceRepository 必须是公开 URI".into()))?;
    if !matches!(source.scheme(), "https" | "http") || source.host_str().is_none() {
        return Err(AssetPublishError::InvalidRequest(
            "sourceRepository 必须是 http 或 https 仓库 URI".into(),
        ));
    }
    validate_public_asset_file("metadata/author.txt", author)?;
    validate_public_asset_file("metadata/license.txt", license)?;
    validate_public_asset_file("metadata/source-repository.txt", source_repository)?;
    Ok(())
}

fn canonical_files(files: Vec<AssetTextFile>) -> Result<Vec<CanonicalAssetFile>, AssetPublishError> {
    let mut seen = std::collections::HashSet::new();
    let mut total = 0_usize;
    let mut canonical = Vec::with_capacity(files.len());
    for file in files {
        let path = safe_relative_path(&file.path)?;
        if path == Path::new("asset-package.json") {
            return Err(AssetPublishError::AssetSanitization(
                "files 不能覆盖 asset-package.json".into(),
            ));
        }
        let normalized = path_to_slashes(&path)?;
        validate_public_asset_file(&normalized, &file.content)?;
        if !seen.insert(normalized.to_ascii_lowercase()) {
            return Err(AssetPublishError::AssetSanitization(format!(
                "文件路径重复或存在大小写冲突：{normalized}"
            )));
        }
        let size = file.content.len();
        if size > MAX_CANONICAL_FILE_BYTES {
            return Err(AssetPublishError::HubPackageTooLarge {
                actual: size as u64,
                limit: MAX_CANONICAL_FILE_BYTES as u64,
            });
        }
        total = total.saturating_add(size);
        if total > MAX_CANONICAL_TOTAL_BYTES {
            return Err(AssetPublishError::HubPackageTooLarge {
                actual: total as u64,
                limit: MAX_CANONICAL_TOTAL_BYTES as u64,
            });
        }
        canonical.push(CanonicalAssetFile {
            path: normalized,
            sha256: format!("sha256-{}", hex::encode(Sha256::digest(file.content.as_bytes()))),
            size: size as u64,
            content: file.content,
        });
    }
    canonical.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(canonical)
}

/// 发布边界的最后一道内容门禁。路径或内容一旦具有凭据、本机路径或
/// 私钥特征就拒绝整个规范包，避免“清洗后继续发布”造成静默泄密。
pub(crate) fn validate_public_asset_file(path: &str, content: &str) -> Result<(), AssetPublishError> {
    let lower_path = path.to_ascii_lowercase();
    let file_name = lower_path.rsplit('/').next().unwrap_or(&lower_path);
    let extension = file_name.rsplit_once('.').map(|(_, extension)| extension);
    if lower_path.split('/').any(|segment| segment.starts_with('.'))
        || matches!(
            file_name,
            "credentials"
                | "credentials.json"
                | "secrets"
                | "secrets.json"
                | "id_rsa"
                | "id_ed25519"
                | "known_hosts"
                | "authorized_keys"
                | "npmrc"
                | "pypirc"
                | "netrc"
                | "git-credentials"
        )
        || matches!(
            extension,
            Some(
                "env"
                    | "pem"
                    | "key"
                    | "p12"
                    | "pfx"
                    | "jks"
                    | "keystore"
                    | "kdbx"
                    | "ovpn"
                    | "mobileprovision"
                    | "toml"
            )
        )
    {
        return Err(AssetPublishError::AssetSanitization(format!(
            "文件 {path} 属于禁止发布的敏感路径或文件类型"
        )));
    }

    if private_key_pattern().is_match(content) {
        return sensitive_content_error(path, "检测到私钥");
    }
    if credential_token_pattern().is_match(content) {
        return sensitive_content_error(path, "检测到访问令牌、Bearer 凭据或云凭据");
    }
    if credential_url_pattern().is_match(content) {
        return sensitive_content_error(path, "检测到 URL 内嵌凭据");
    }
    if local_path_pattern().is_match(content) {
        return sensitive_content_error(path, "检测到本机用户目录");
    }
    let mut search_start = 0;
    while search_start < content.len() {
        let Some(captures) = secret_assignment_pattern().captures_at(content, search_start) else {
            break;
        };
        let Some(whole_match) = captures.get(0) else {
            break;
        };
        let Some(key) = captures.name("key").map(|key| key.as_str()) else {
            search_start = whole_match.end();
            continue;
        };
        if !is_sensitive_key(key) {
            search_start = captures
                .name("key")
                .map_or(whole_match.end(), |key_match| key_match.end())
                .max(search_start.saturating_add(1));
            continue;
        }
        let Some(value) = captures.name("value").map(|value| value.as_str()) else {
            search_start = whole_match.end();
            continue;
        };
        if !is_safe_secret_placeholder(value) {
            return sensitive_content_error(path, "检测到疑似明文密钥配置");
        }
        search_start = whole_match.end();
    }
    Ok(())
}

fn sensitive_content_error<T>(path: &str, reason: &str) -> Result<T, AssetPublishError> {
    Err(AssetPublishError::AssetSanitization(format!(
        "文件 {path} 未通过敏感信息扫描：{reason}"
    )))
}

fn is_safe_secret_placeholder(value: &str) -> bool {
    let value = value
        .trim_matches(|character: char| matches!(character, '"' | '\'' | '`' | ',' | ';'))
        .trim();
    let uppercase = value.to_ascii_uppercase();
    value.is_empty()
        || value == "***"
        || placeholder_stars_pattern().is_match(value)
        || placeholder_env_pattern().is_match(value)
        || placeholder_template_pattern().is_match(value)
        || placeholder_angle_pattern().is_match(value)
        || placeholder_process_env_pattern().is_match(value)
        || placeholder_powershell_env_pattern().is_match(value)
        || uppercase == "CHANGEME"
        || uppercase == "EXAMPLE"
        || uppercase == "REPLACE_ME"
        || uppercase == "REDACTED"
}

fn private_key_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"(?i)-----BEGIN (?:RSA |EC |OPENSSH |DSA )?PRIVATE KEY-----").expect("private key regex")
    })
}

fn credential_token_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(
            r"(?i)\b(?:sk-(?:proj-)?[a-z0-9_-]{16,}|gh[pousr]_[a-z0-9]{20,}|github_pat_[a-z0-9_]{20,}|AKIA[0-9A-Z]{16}|Bearer\s+[a-z0-9._~+/-]{20,}=?)\b",
        )
        .expect("credential token regex")
    })
}

fn credential_url_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(
            r"(?i)(?:[a-z][a-z0-9+.-]*://[^/\s:@]+:[^/\s@]+@|[?&](?:api[_-]?key|token|access[_-]?token|secret|password)=[^&#\s]{4,})",
        )
            .expect("credential URL regex")
    })
}

fn local_path_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(
            r"(?i)(?:[a-z]:[\\/](?:users|documents and settings)[\\/][^\\/\s]+|/(?:users|home)/[^/\s]+/|\\\\[^\\\s]+\\[^\\\s]+)",
        )
        .expect("local path regex")
    })
}

fn secret_assignment_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(
            r#"(?im)["']?(?P<key>[a-z0-9][a-z0-9_. -]{0,127})["']?\s*[:=]\s*(?P<value>"[^"\r\n]*"|'[^'\r\n]*'|`[^`\r\n]*`|\$\{[A-Za-z_][A-Za-z0-9_]*\}|\{\{[^{}\r\n]+\}\}|<[^<>\r\n]+>|[^\s#,}\]]+)"#,
        )
        .expect("secret assignment regex")
    })
}

fn sensitive_key_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(
            r"(?:^|_)(?:api_?key|access_?token|auth_?token|authorization|client_?secret|password|passwd|private_?key|refresh_?token|secret)(?:$|_)",
        )
        .expect("sensitive key regex")
    })
}

fn placeholder_stars_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"^\*+$").expect("stars placeholder regex"))
}

fn placeholder_env_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"^\$\{[A-Za-z_][A-Za-z0-9_]*\}$").expect("environment placeholder regex"))
}

fn placeholder_template_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"^\{\{[^{}]+\}\}$").expect("template placeholder regex"))
}

fn placeholder_angle_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"^<[^<>]+>$").expect("angle placeholder regex"))
}

fn placeholder_process_env_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN
        .get_or_init(|| Regex::new(r"^process\.env\.[A-Za-z_][A-Za-z0-9_]*$").expect("process env placeholder regex"))
}

fn placeholder_powershell_env_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"(?i)^\$env:[A-Za-z_][A-Za-z0-9_]*$").expect("PowerShell env placeholder regex"))
}

fn is_sensitive_key(key: &str) -> bool {
    let mut canonical = String::with_capacity(key.len());
    let mut previous_was_lower_or_digit = false;
    let mut previous_was_separator = true;
    for character in key.chars() {
        if character.is_ascii_uppercase() {
            if previous_was_lower_or_digit && !previous_was_separator {
                canonical.push('_');
            }
            canonical.push(character.to_ascii_lowercase());
            previous_was_lower_or_digit = false;
            previous_was_separator = false;
        } else if character.is_ascii_lowercase() || character.is_ascii_digit() {
            canonical.push(character);
            previous_was_lower_or_digit = true;
            previous_was_separator = false;
        } else if !previous_was_separator && !canonical.is_empty() {
            canonical.push('_');
            previous_was_lower_or_digit = false;
            previous_was_separator = true;
        }
    }
    let canonical = canonical.trim_matches('_');
    matches!(canonical, "env" | "headers" | "envoverride" | "env_override" | "token")
        || sensitive_key_pattern().is_match(canonical)
}

fn validate_package_name(value: &str) -> Result<(), AssetPublishError> {
    if !(12..=96).contains(&value.len()) || !value.starts_with("tjuaeasset-") {
        return Err(AssetPublishError::InvalidRequest(
            "packageName 必须使用 tjuaeasset- 前缀".into(),
        ));
    }
    let suffix = &value["tjuaeasset-".len()..];
    if suffix.is_empty()
        || suffix.starts_with('-')
        || suffix.ends_with('-')
        || suffix.contains("--")
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(AssetPublishError::InvalidRequest(
            "packageName 不是合法 kebab-case".into(),
        ));
    }
    Ok(())
}

fn validate_asset_local_id(value: &str, field: &str) -> Result<(), AssetPublishError> {
    if value.is_empty() || value.len() > 128 {
        return Err(AssetPublishError::InvalidRequest(format!("{field} 长度无效")));
    }
    let mut expect_alphanumeric = true;
    for byte in value.bytes() {
        let alphanumeric = byte.is_ascii_lowercase() || byte.is_ascii_digit();
        if expect_alphanumeric {
            if !alphanumeric {
                return Err(AssetPublishError::InvalidRequest(format!(
                    "{field} 必须使用小写可移植标识符"
                )));
            }
            expect_alphanumeric = false;
        } else if matches!(byte, b'.' | b'_' | b':' | b'-') {
            expect_alphanumeric = true;
        } else if !alphanumeric {
            return Err(AssetPublishError::InvalidRequest(format!(
                "{field} 必须使用小写可移植标识符"
            )));
        }
    }
    if expect_alphanumeric {
        return Err(AssetPublishError::InvalidRequest(format!("{field} 不能以分隔符结尾")));
    }
    Ok(())
}

fn validate_asset_text(value: &str, min: usize, max: usize, field: &str) -> Result<(), AssetPublishError> {
    let length = value.chars().count();
    if !(min..=max).contains(&length) || value.chars().any(char::is_control) {
        return Err(AssetPublishError::InvalidRequest(format!(
            "{field} 必须是 {min} 到 {max} 个不含控制字符的字符"
        )));
    }
    Ok(())
}

fn validate_dependencies(dependencies: &[String]) -> Result<(), AssetPublishError> {
    if dependencies.len() > 128 {
        return Err(AssetPublishError::InvalidRequest("dependencies 最多允许 128 项".into()));
    }
    let mut seen = std::collections::HashSet::new();
    for dependency in dependencies {
        if !remote_asset_id_pattern().is_match(dependency) {
            return Err(AssetPublishError::InvalidRequest(format!(
                "依赖不是不可变的 TjuaeHub 资产 ID：{dependency}"
            )));
        }
        if !seen.insert(dependency) {
            return Err(AssetPublishError::InvalidRequest(format!("依赖重复：{dependency}")));
        }
    }
    Ok(())
}

fn remote_asset_id_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(
            r"^tjuaeasset-[a-z0-9]+(?:-[a-z0-9]+)*/(?:assistant|engineAdapter|skill|mcp)/[a-z0-9]+(?:[._:-][a-z0-9]+)*$",
        )
        .expect("remote asset id regex")
    })
}

fn asset_kind_name(kind: &HubAssetKind) -> &'static str {
    match kind {
        HubAssetKind::Assistant => "assistant",
        HubAssetKind::EngineAdapter => "engineAdapter",
        HubAssetKind::Skill => "skill",
        HubAssetKind::Mcp => "mcp",
    }
}

fn definition_file_for_kind(kind: &HubAssetKind) -> &'static str {
    match kind {
        HubAssetKind::Assistant => "assistant.json",
        HubAssetKind::EngineAdapter => "engine-adapter.json",
        HubAssetKind::Skill => "SKILL.md",
        HubAssetKind::Mcp => "mcp.json",
    }
}

pub(crate) fn safe_relative_path(value: &str) -> Result<PathBuf, AssetPublishError> {
    if value.is_empty()
        || value.len() > 4096
        || value.contains('\\')
        || value.contains(':')
        || value.starts_with('/')
        || value.contains('\0')
        || value.chars().any(char::is_control)
    {
        return Err(AssetPublishError::AssetSanitization(format!("文件路径不安全：{value}")));
    }
    let path = Path::new(value);
    for component in path.components() {
        let Component::Normal(part) = component else {
            return Err(AssetPublishError::AssetSanitization(format!("文件路径不安全：{value}")));
        };
        let part = part
            .to_str()
            .ok_or_else(|| AssetPublishError::AssetSanitization("文件路径不是有效 UTF-8".into()))?;
        if !is_portable_file_name(part) {
            return Err(AssetPublishError::AssetSanitization(format!(
                "文件路径不适合跨平台发布：{value}"
            )));
        }
    }
    Ok(path.to_path_buf())
}

fn is_portable_file_name(value: &str) -> bool {
    if value.is_empty()
        || value.len() > 255
        || value.ends_with(['.', ' '])
        || value
            .chars()
            .any(|character| matches!(character, '<' | '>' | '"' | '|' | '?' | '*'))
    {
        return false;
    }
    let stem = value
        .split('.')
        .next()
        .unwrap_or(value)
        .trim_end_matches(['.', ' '])
        .to_ascii_uppercase();
    !matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "CLOCK$"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

fn path_to_slashes(path: &Path) -> Result<String, AssetPublishError> {
    path.components()
        .map(|component| match component {
            Component::Normal(value) => value
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| AssetPublishError::AssetSanitization("文件路径不是有效 UTF-8".into())),
            _ => Err(AssetPublishError::AssetSanitization("文件路径无效".into())),
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|parts| parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_preview_is_a_single_declarative_asset_package() {
        let canonical = canonicalize(
            HubAssetKind::Mcp,
            "demo",
            "tjuaeasset-demo",
            "1.0.0",
            PublicationMetadata {
                author: "Demo Author",
                license: "MIT",
                source_repository: "https://github.com/example/demo",
                tags: &["mcp".into()],
                confirmed: true,
            },
            LocalAssetMaterial {
                display_name: "演示 MCP".into(),
                description: "用于验证发布预览的安全清理结果。".into(),
                runtime_id: "demo".into(),
                definition_file: "mcp.json".into(),
                dependencies: Vec::new(),
                files: vec![AssetTextFile {
                    path: "mcp.json".into(),
                    content: r#"{"kind":"mcp","runtimeId":"demo"}"#.into(),
                }],
                blocked_fields: Vec::new(),
            },
        )
        .unwrap();

        assert!(canonical.warning_codes.is_empty());
        assert_eq!(canonical.package.package_name, "tjuaeasset-demo");
        assert_eq!(canonical.package.manifest["schemaVersion"], 1);
        assert_eq!(canonical.package.manifest["assets"].as_array().unwrap().len(), 1);
        assert_eq!(canonical.package.manifest["assets"][0]["definitionFile"], "mcp.json");
        assert!(canonical.package.manifest.get("contributes").is_none());
        assert!(canonical.package.manifest.get("lifecycle").is_none());
    }

    #[test]
    fn each_asset_kind_requires_its_canonical_definition_file() {
        let material = |definition_file: &str| LocalAssetMaterial {
            display_name: "演示引擎".into(),
            description: "用于验证引擎适配器固定入口。".into(),
            runtime_id: "demo".into(),
            definition_file: definition_file.into(),
            dependencies: Vec::new(),
            files: vec![AssetTextFile {
                path: definition_file.into(),
                content: "{}".into(),
            }],
            blocked_fields: Vec::new(),
        };

        assert!(
            canonicalize(
                HubAssetKind::EngineAdapter,
                "demo",
                "tjuaeasset-demo",
                "1.0.0",
                PublicationMetadata {
                    author: "Demo Author",
                    license: "MIT",
                    source_repository: "https://github.com/example/demo",
                    tags: &[],
                    confirmed: true,
                },
                material("engine-adapter.json"),
            )
            .is_ok()
        );
        assert!(
            canonicalize(
                HubAssetKind::EngineAdapter,
                "demo",
                "tjuaeasset-demo",
                "1.0.0",
                PublicationMetadata {
                    author: "Demo Author",
                    license: "MIT",
                    source_repository: "https://github.com/example/demo",
                    tags: &[],
                    confirmed: true,
                },
                material("assistant.json"),
            )
            .is_err()
        );
    }

    #[test]
    fn canonical_package_uses_only_explicitly_confirmed_legal_metadata() {
        let material = || LocalAssetMaterial {
            display_name: "演示技能".into(),
            description: "验证作者与许可证不会被平台伪造。".into(),
            runtime_id: "demo".into(),
            definition_file: "SKILL.md".into(),
            dependencies: Vec::new(),
            files: vec![AssetTextFile {
                path: "SKILL.md".into(),
                content: "# Demo".into(),
            }],
            blocked_fields: Vec::new(),
        };

        for (author, license, confirmed) in [
            ("", "MIT", true),
            ("Demo Author", "", true),
            ("Demo Author", "MIT", false),
        ] {
            assert!(
                canonicalize(
                    HubAssetKind::Skill,
                    "demo",
                    "tjuaeasset-demo",
                    "1.0.0",
                    PublicationMetadata {
                        author,
                        license,
                        source_repository: "https://github.com/example/demo",
                        tags: &[],
                        confirmed,
                    },
                    material(),
                )
                .is_err()
            );
        }

        let canonical = canonicalize(
            HubAssetKind::Skill,
            "demo",
            "tjuaeasset-demo",
            "1.0.0",
            PublicationMetadata {
                author: "  Demo Author  ",
                license: "  MPL-2.0  ",
                source_repository: "https://github.com/example/demo",
                tags: &[],
                confirmed: true,
            },
            material(),
        )
        .unwrap();
        assert_eq!(canonical.package.manifest["author"], "Demo Author");
        assert_eq!(canonical.package.manifest["license"], "MPL-2.0");
    }

    #[test]
    fn canonical_files_reject_traversal_and_case_collisions() {
        assert!(
            canonical_files(vec![AssetTextFile {
                path: "../escape".into(),
                content: "x".into(),
            }])
            .is_err()
        );
        assert!(
            canonical_files(vec![
                AssetTextFile {
                    path: "A.txt".into(),
                    content: "a".into(),
                },
                AssetTextFile {
                    path: "a.txt".into(),
                    content: "b".into(),
                },
            ])
            .is_err()
        );
    }

    #[test]
    fn public_asset_gate_rejects_sensitive_paths_and_content() {
        for path in [
            "skills/demo/.env",
            "skills/demo/config.toml",
            "skills/demo/client.pem",
            "skills/demo/id_rsa",
        ] {
            assert!(validate_public_asset_file(path, "harmless").is_err(), "{path}");
        }
        for content in [
            "-----BEGIN PRIVATE KEY-----\nsecret",
            "OPENAI_API_KEY=sk-proj-abcdefghijklmnopqrstuvwxyz",
            "password=hunter2-secret",
            "refreshToken=literal-refresh-credential",
            "secret-value: literal-secret-value",
            r#"{"clientSecret":"literal-client-secret"}"#,
            r#"{"description":"example refreshToken=literal-refresh-credential"}"#,
            "refreshToken=YOUR_REAL_PASSWORD_hunter2",
            "secretValue=abcREDACTEDactual-secret",
            "Authorization: Bearer abcdefghijklmnopqrstuvwxyz",
            "https://user:password@example.com/api",
            "读取 C:\\Users\\alice\\secret.txt",
        ] {
            let assignments = secret_assignment_pattern()
                .captures_iter(content)
                .map(|captures| {
                    (
                        captures.name("key").unwrap().as_str().to_owned(),
                        captures.name("value").unwrap().as_str().to_owned(),
                    )
                })
                .collect::<Vec<_>>();
            assert!(
                validate_public_asset_file("skills/demo/SKILL.md", content).is_err(),
                "{content}: {assignments:?}"
            );
        }
    }

    #[test]
    fn public_asset_gate_allows_documented_secret_placeholders() {
        let content = concat!(
            "OPENAI_API_KEY=${OPENAI_API_KEY}\n",
            "password=<在本机配置>\n",
            "refreshToken={{REFRESH_TOKEN}}\n",
            "client-secret=process.env.CLIENT_SECRET\n",
            "password=REDACTED\n",
            r#"{"secretValue":"REPLACE_ME","powershellSecret":"$env:SECRET_VALUE"}"#
        );
        assert!(
            validate_public_asset_file("skills/demo/SKILL.md", content).is_ok(),
            "{:?}",
            secret_assignment_pattern()
                .captures_iter(content)
                .map(|captures| (
                    captures.name("key").unwrap().as_str(),
                    captures.name("value").unwrap().as_str()
                ))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn public_asset_gate_rejects_unclosed_or_overbroad_placeholders() {
        for content in [
            "refreshToken=${REFRESH_TOKEN",
            "secretValue={{SECRET_VALUE",
            "password=<在本机配置",
            "apiKey=VALUE",
            "clientSecret=NULL",
        ] {
            assert!(
                validate_public_asset_file("skills/demo/SKILL.md", content).is_err(),
                "{content}"
            );
        }
    }

    #[test]
    fn sensitive_key_normalization_covers_camel_case_and_separators() {
        for key in [
            "refreshToken",
            "refresh-token",
            "refresh.token",
            "secretValue",
            "client secret",
            "private_key",
        ] {
            assert!(is_sensitive_key(key), "{key}");
        }
        assert!(!is_sensitive_key("description"));
    }

    #[test]
    fn portable_asset_paths_reject_windows_device_names_and_ads() {
        for path in [
            "skills/demo/CON.txt",
            "skills/demo/file.txt:secret",
            "skills/demo/trailing.",
        ] {
            assert!(safe_relative_path(path).is_err(), "{path}");
        }
    }
}
