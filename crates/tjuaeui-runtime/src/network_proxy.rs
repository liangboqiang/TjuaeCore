//! 统一网络代理策略。
//!
//! 本模块是 Core 进程内所有出站网络路径的唯一代理策略来源：
//!
//! - [`crate::Builder`] 在真正启动子进程前应用代理环境变量，因此 Codex、
//!   Claude、Gemini、OpenCode、自定义 ACP Agent 等都走同一条路径；
//! - Core 自己创建的 `reqwest::Client` 通过
//!   [`apply_network_proxy_to_http_client`] 使用同一策略；
//! - “不使用代理”会显式清除继承的代理变量，避免宿主环境污染智能体。
//!
//! 策略只保存在进程内，不修改 TjuaeCore 自身的全局环境变量。

use std::sync::{LazyLock, RwLock};

use reqwest::{ClientBuilder, Proxy};
use url::Url;

const DEFAULT_NO_PROXY: &str = "localhost,127.0.0.1,::1";
const PROXY_ENV_KEYS: &[&str] = &[
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "WS_PROXY",
    "WSS_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
    "ws_proxy",
    "wss_proxy",
];

static NETWORK_PROXY_CONFIG: LazyLock<RwLock<NetworkProxyConfig>> =
    LazyLock::new(|| RwLock::new(NetworkProxyConfig::default()));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkProxyMode {
    FollowSystem,
    Manual,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkProxyConfig {
    pub mode: NetworkProxyMode,
    pub proxy_url: Option<String>,
    pub no_proxy: String,
}

impl Default for NetworkProxyConfig {
    fn default() -> Self {
        Self {
            mode: NetworkProxyMode::FollowSystem,
            proxy_url: None,
            no_proxy: DEFAULT_NO_PROXY.to_owned(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkProxyState {
    Active,
    Direct,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkProxySource {
    Manual,
    Environment,
    WindowsSystem,
    Disabled,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkProxyWarning {
    PacUnsupported,
    InvalidSystemProxy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkProxyStatus {
    pub mode: NetworkProxyMode,
    pub state: NetworkProxyState,
    pub source: NetworkProxySource,
    pub proxy_url: Option<String>,
    pub no_proxy: String,
    pub warning: Option<NetworkProxyWarning>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ResolvedProxyEnvironment {
    http_proxy: Option<String>,
    https_proxy: Option<String>,
    all_proxy: Option<String>,
    no_proxy: String,
    source: Option<NetworkProxySource>,
    warning: Option<NetworkProxyWarning>,
}

impl ResolvedProxyEnvironment {
    fn primary_proxy_url(&self) -> Option<String> {
        self.https_proxy
            .clone()
            .or_else(|| self.http_proxy.clone())
            .or_else(|| self.all_proxy.clone())
    }

    fn status(&self, mode: NetworkProxyMode) -> NetworkProxyStatus {
        let proxy_url = self.primary_proxy_url();
        NetworkProxyStatus {
            mode,
            state: if proxy_url.is_some() {
                NetworkProxyState::Active
            } else {
                NetworkProxyState::Direct
            },
            source: self.source.unwrap_or(NetworkProxySource::None),
            proxy_url,
            no_proxy: self.no_proxy.clone(),
            warning: self.warning,
        }
    }
}

/// 更新进程内代理策略并返回立即解析后的状态。
///
/// 手动代理仅接受不带凭据的 `http://` 或 `https://` URL。认证信息不应以
/// 明文形式写入系统设置数据库。
pub fn set_network_proxy_config(mut config: NetworkProxyConfig) -> Result<NetworkProxyStatus, String> {
    config = validate_network_proxy_config(config)?;

    *NETWORK_PROXY_CONFIG
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = config;

    let status = network_proxy_status();
    tracing::info!(
        mode = ?status.mode,
        state = ?status.state,
        source = ?status.source,
        has_proxy = status.proxy_url.is_some(),
        warning = ?status.warning,
        "network proxy policy updated"
    );
    Ok(status)
}

/// 校验并规范化代理配置，但不修改当前运行时策略。
pub fn validate_network_proxy_config(mut config: NetworkProxyConfig) -> Result<NetworkProxyConfig, String> {
    config.no_proxy = normalize_no_proxy(&config.no_proxy);
    config.proxy_url = match config.mode {
        NetworkProxyMode::Manual => Some(normalize_proxy_url(
            config
                .proxy_url
                .as_deref()
                .ok_or_else(|| "手动代理模式需要代理地址".to_owned())?,
        )?),
        NetworkProxyMode::FollowSystem | NetworkProxyMode::Disabled => None,
    };

    Ok(config)
}

pub fn network_proxy_status() -> NetworkProxyStatus {
    let config = current_config();
    resolve_proxy_environment(&config).status(config.mode)
}

/// 将当前代理策略应用到即将启动的子进程。
///
/// 该函数在 `spawn()` / `output()` 的最后一刻调用，因此即使调用方此前执行了
/// `env_clear()`，代理策略仍会覆盖全部智能体启动路径。
pub(crate) fn apply_network_proxy_to_command(command: &mut tokio::process::Command) {
    for key in PROXY_ENV_KEYS {
        command.env_remove(key);
    }

    let config = current_config();
    let resolved = resolve_proxy_environment(&config);
    if let Some(proxy) = resolved.http_proxy.as_deref() {
        set_command_proxy_env(command, "HTTP_PROXY", "http_proxy", proxy);
        set_command_proxy_env(command, "WS_PROXY", "ws_proxy", proxy);
    }
    if let Some(proxy) = resolved.https_proxy.as_deref() {
        set_command_proxy_env(command, "HTTPS_PROXY", "https_proxy", proxy);
        set_command_proxy_env(command, "WSS_PROXY", "wss_proxy", proxy);
    }
    if let Some(proxy) = resolved.all_proxy.as_deref() {
        set_command_proxy_env(command, "ALL_PROXY", "all_proxy", proxy);
    }
    set_command_proxy_env(command, "NO_PROXY", "no_proxy", resolved.no_proxy.as_str());
}

/// 为 Core 自身的 HTTP 客户端安装动态代理选择器。
///
/// 选择器会在每次请求时读取当前策略，因此用户修改设置后，不需要重建已经注入到
/// 各领域服务中的 `reqwest::Client`。
pub fn apply_network_proxy_to_http_client(builder: ClientBuilder) -> ClientBuilder {
    builder.proxy(Proxy::custom(proxy_url_for_request))
}

/// 返回指定目标在当前策略下应使用的代理 URL。
///
/// 非 HTTP 客户端（例如远程智能体 WebSocket）通过此入口复用相同的模式、系统
/// 代理解析和直连白名单规则。
pub fn network_proxy_url_for(target: &Url) -> Option<Url> {
    proxy_url_for_request(target)
}

fn proxy_url_for_request(target: &Url) -> Option<Url> {
    let config = current_config();
    let resolved = resolve_proxy_environment(&config);
    if should_bypass_proxy(target, &resolved.no_proxy) {
        return None;
    }

    let value = match target.scheme() {
        "http" | "ws" => resolved.http_proxy.or(resolved.all_proxy),
        "https" | "wss" => resolved.https_proxy.or(resolved.all_proxy),
        _ => resolved.all_proxy,
    }?;
    Url::parse(&value).ok()
}

fn current_config() -> NetworkProxyConfig {
    NETWORK_PROXY_CONFIG
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

fn resolve_proxy_environment(config: &NetworkProxyConfig) -> ResolvedProxyEnvironment {
    match config.mode {
        NetworkProxyMode::Disabled => ResolvedProxyEnvironment {
            no_proxy: normalize_no_proxy(&config.no_proxy),
            source: Some(NetworkProxySource::Disabled),
            ..Default::default()
        },
        NetworkProxyMode::Manual => {
            let proxy_url = config.proxy_url.clone();
            ResolvedProxyEnvironment {
                http_proxy: proxy_url.clone(),
                https_proxy: proxy_url,
                no_proxy: normalize_no_proxy(&config.no_proxy),
                source: Some(NetworkProxySource::Manual),
                ..Default::default()
            }
        }
        NetworkProxyMode::FollowSystem => resolve_system_proxy(config),
    }
}

fn resolve_system_proxy(config: &NetworkProxyConfig) -> ResolvedProxyEnvironment {
    if let Some(mut resolved) = proxy_from_environment() {
        resolved.no_proxy = merge_no_proxy(&resolved.no_proxy, &config.no_proxy);
        return resolved;
    }

    #[cfg(windows)]
    {
        let mut resolved = windows_system_proxy();
        resolved.no_proxy = merge_no_proxy(&resolved.no_proxy, &config.no_proxy);
        resolved
    }

    #[cfg(not(windows))]
    ResolvedProxyEnvironment {
        no_proxy: normalize_no_proxy(&config.no_proxy),
        source: Some(NetworkProxySource::None),
        ..Default::default()
    }
}

fn proxy_from_environment() -> Option<ResolvedProxyEnvironment> {
    let http_proxy = env_value("HTTP_PROXY");
    let https_proxy = env_value("HTTPS_PROXY");
    let all_proxy = env_value("ALL_PROXY");
    if http_proxy.is_none() && https_proxy.is_none() && all_proxy.is_none() {
        return None;
    }

    Some(ResolvedProxyEnvironment {
        http_proxy: http_proxy.and_then(|value| normalize_proxy_url(&value).ok()),
        https_proxy: https_proxy.and_then(|value| normalize_proxy_url(&value).ok()),
        all_proxy: all_proxy.and_then(|value| normalize_proxy_url(&value).ok()),
        no_proxy: env_value("NO_PROXY").unwrap_or_default(),
        source: Some(NetworkProxySource::Environment),
        warning: None,
    })
}

fn env_value(uppercase: &str) -> Option<String> {
    let lowercase = uppercase.to_ascii_lowercase();
    std::env::var(uppercase)
        .ok()
        .or_else(|| std::env::var(lowercase).ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn normalize_proxy_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("代理地址不能为空".to_owned());
    }
    if trimmed.contains(['\r', '\n']) {
        return Err("代理地址不能包含换行符".to_owned());
    }

    let candidate = if trimmed.contains("://") {
        trimmed.to_owned()
    } else {
        format!("http://{trimmed}")
    };
    let mut parsed = Url::parse(&candidate).map_err(|_| "代理地址格式无效".to_owned())?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("代理地址仅支持 http 或 https 协议".to_owned());
    }
    if parsed.host_str().is_none() {
        return Err("代理地址缺少主机名".to_owned());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("代理地址暂不接受明文用户名或密码".to_owned());
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err("代理地址不能包含查询参数或片段".to_owned());
    }
    if parsed.path() != "/" && !parsed.path().is_empty() {
        return Err("代理地址不能包含路径".to_owned());
    }

    parsed.set_path("");
    Ok(parsed.to_string().trim_end_matches('/').to_owned())
}

fn normalize_no_proxy(raw: &str) -> String {
    let mut values = Vec::new();
    for value in DEFAULT_NO_PROXY
        .split(',')
        .chain(raw.split([',', ';']))
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if !values.contains(&value) {
            values.push(value);
        }
    }
    values.join(",")
}

fn merge_no_proxy(first: &str, second: &str) -> String {
    normalize_no_proxy(&format!("{first},{second}"))
}

fn should_bypass_proxy(target: &Url, no_proxy: &str) -> bool {
    let Some(host) = target.host_str() else {
        return false;
    };
    let host = host.trim_matches(['[', ']']).to_ascii_lowercase();
    let port = target.port_or_known_default();

    no_proxy.split([',', ';']).map(str::trim).any(|entry| {
        if entry.is_empty() {
            return false;
        }
        if entry == "*" {
            return true;
        }
        if entry.eq_ignore_ascii_case("<local>") {
            return !host.contains('.');
        }

        let (entry_host, entry_port) = split_no_proxy_host_port(entry);
        if entry_port.is_some() && entry_port != port {
            return false;
        }
        let normalized = entry_host
            .trim_start_matches("*.")
            .trim_start_matches('.')
            .trim_matches(['[', ']'])
            .to_ascii_lowercase();
        host == normalized || host.ends_with(&format!(".{normalized}"))
    })
}

fn split_no_proxy_host_port(value: &str) -> (&str, Option<u16>) {
    if value.starts_with('[')
        && let Some(end) = value.find(']')
    {
        let port = value
            .get(end + 1..)
            .and_then(|suffix| suffix.strip_prefix(':'))
            .and_then(|port| port.parse().ok());
        return (&value[1..end], port);
    }

    match value.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => (host, port.parse().ok()),
        _ => (value, None),
    }
}

fn set_command_proxy_env(command: &mut tokio::process::Command, uppercase: &str, lowercase: &str, value: &str) {
    command.env(uppercase, value);
    #[cfg(not(windows))]
    command.env(lowercase, value);
    #[cfg(windows)]
    let _ = lowercase;
}

#[cfg(windows)]
fn windows_system_proxy() -> ResolvedProxyEnvironment {
    let enabled = read_windows_registry_dword("ProxyEnable").unwrap_or(0) != 0;
    let proxy_server = read_windows_registry_string("ProxyServer").unwrap_or_default();
    let proxy_override = read_windows_registry_string("ProxyOverride").unwrap_or_default();
    let pac_url = read_windows_registry_string("AutoConfigURL").unwrap_or_default();

    if enabled && !proxy_server.trim().is_empty() {
        let mut resolved = parse_windows_proxy_server(&proxy_server);
        resolved.no_proxy = proxy_override;
        resolved.source = Some(NetworkProxySource::WindowsSystem);
        if resolved.primary_proxy_url().is_none() {
            resolved.warning = Some(NetworkProxyWarning::InvalidSystemProxy);
        }
        return resolved;
    }

    ResolvedProxyEnvironment {
        no_proxy: proxy_override,
        source: Some(NetworkProxySource::None),
        warning: (!pac_url.trim().is_empty()).then_some(NetworkProxyWarning::PacUnsupported),
        ..Default::default()
    }
}

#[cfg(any(windows, test))]
fn parse_windows_proxy_server(raw: &str) -> ResolvedProxyEnvironment {
    let trimmed = raw.trim();
    if !trimmed.contains('=') {
        let proxy = normalize_proxy_url(trimmed).ok();
        return ResolvedProxyEnvironment {
            http_proxy: proxy.clone(),
            https_proxy: proxy,
            ..Default::default()
        };
    }

    let mut resolved = ResolvedProxyEnvironment::default();
    for segment in trimmed.split(';') {
        let Some((scheme, value)) = segment.split_once('=') else {
            continue;
        };
        let proxy = normalize_proxy_url(value).ok();
        match scheme.trim().to_ascii_lowercase().as_str() {
            "http" => resolved.http_proxy = proxy,
            "https" => resolved.https_proxy = proxy,
            "socks" | "socks5" => resolved.all_proxy = proxy,
            _ => {}
        }
    }
    if resolved.http_proxy.is_none() {
        resolved.http_proxy = resolved.https_proxy.clone();
    }
    if resolved.https_proxy.is_none() {
        resolved.https_proxy = resolved.http_proxy.clone();
    }
    resolved
}

#[cfg(windows)]
fn read_windows_registry_dword(value_name: &str) -> Option<u32> {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};

    let subkey = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings");
    let value_name = wide(value_name);
    let mut value = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    let result = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value_name.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut value as *mut u32).cast(),
            &mut size,
        )
    };
    (result == windows_sys::Win32::Foundation::ERROR_SUCCESS).then_some(value)
}

#[cfg(windows)]
fn read_windows_registry_string(value_name: &str) -> Option<String> {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW};

    let subkey = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings");
    let value_name = wide(value_name);
    let mut size = 0u32;
    let first = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value_name.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        )
    };
    if first != windows_sys::Win32::Foundation::ERROR_SUCCESS || size < 2 {
        return None;
    }

    let mut buffer = vec![0u16; (size as usize).div_ceil(2)];
    let second = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value_name.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if second != windows_sys::Win32::Foundation::ERROR_SUCCESS {
        return None;
    }
    let len = buffer.iter().position(|ch| *ch == 0).unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..len]))
}

#[cfg(windows)]
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::ffi::OsStr;
    use std::sync::Mutex;

    use super::*;

    static CONFIG_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn manual_proxy_normalizes_url_and_local_bypass() {
        assert_eq!(normalize_proxy_url("127.0.0.1:7897").unwrap(), "http://127.0.0.1:7897");
        assert_eq!(
            normalize_no_proxy("internal.example"),
            "localhost,127.0.0.1,::1,internal.example"
        );
    }

    #[test]
    fn proxy_url_rejects_credentials_and_unsupported_protocols() {
        assert!(normalize_proxy_url("http://user:secret@proxy.example:8080").is_err());
        assert!(normalize_proxy_url("socks5://127.0.0.1:1080").is_err());
    }

    #[test]
    fn windows_single_proxy_applies_to_http_and_https() {
        let resolved = parse_windows_proxy_server("127.0.0.1:7897");
        assert_eq!(resolved.http_proxy.as_deref(), Some("http://127.0.0.1:7897"));
        assert_eq!(resolved.https_proxy.as_deref(), Some("http://127.0.0.1:7897"));
    }

    #[test]
    fn windows_protocol_map_keeps_distinct_endpoints() {
        let resolved = parse_windows_proxy_server("http=proxy.example:8080;https=secure-proxy.example:8443");
        assert_eq!(resolved.http_proxy.as_deref(), Some("http://proxy.example:8080"));
        assert_eq!(
            resolved.https_proxy.as_deref(),
            Some("http://secure-proxy.example:8443")
        );
    }

    #[test]
    fn no_proxy_matches_local_exact_suffix_and_port() {
        assert!(should_bypass_proxy(
            &Url::parse("http://localhost:13400/api").unwrap(),
            DEFAULT_NO_PROXY
        ));
        assert!(should_bypass_proxy(
            &Url::parse("https://api.internal.example").unwrap(),
            ".internal.example"
        ));
        assert!(should_bypass_proxy(
            &Url::parse("https://example.com:8443").unwrap(),
            "example.com:8443"
        ));
        assert!(!should_bypass_proxy(
            &Url::parse("https://example.com:443").unwrap(),
            "example.com:8443"
        ));
    }

    #[test]
    fn manual_policy_is_applied_after_command_environment_is_cleared() {
        let _guard = CONFIG_TEST_LOCK.lock().expect("config test lock");
        set_network_proxy_config(NetworkProxyConfig {
            mode: NetworkProxyMode::Manual,
            proxy_url: Some("127.0.0.1:7897".to_owned()),
            no_proxy: "internal.example".to_owned(),
        })
        .expect("manual proxy config");

        let mut command = tokio::process::Command::new("proxy-env-test");
        command.env_clear();
        apply_network_proxy_to_command(&mut command);
        let environment: HashMap<_, _> = command
            .as_std()
            .get_envs()
            .filter_map(|(key, value)| value.map(|value| (key.to_owned(), value.to_owned())))
            .collect();

        assert_eq!(
            environment
                .get(OsStr::new("HTTP_PROXY"))
                .and_then(|value| value.to_str()),
            Some("http://127.0.0.1:7897")
        );
        assert_eq!(
            environment
                .get(OsStr::new("HTTPS_PROXY"))
                .and_then(|value| value.to_str()),
            Some("http://127.0.0.1:7897")
        );
        assert!(
            environment
                .get(OsStr::new("NO_PROXY"))
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.contains("internal.example"))
        );

        set_network_proxy_config(NetworkProxyConfig::default()).expect("restore proxy config");
    }
}
