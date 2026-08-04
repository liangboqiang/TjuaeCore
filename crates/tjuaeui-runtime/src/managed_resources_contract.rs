use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Component, Path, PathBuf};

pub const MANAGED_RESOURCES_CONTRACT_FILE: &str = "manifest.json";
pub const MANAGED_RESOURCES_CONTRACT_SCHEMA_VERSION: u8 = 3;
const SUPPORTED_RUNTIME_KEYS: [&str; 6] = [
    "win32-x64",
    "win32-arm64",
    "darwin-x64",
    "darwin-arm64",
    "linux-x64",
    "linux-arm64",
];

/// Internal runtimes shipped by Tjuae.
///
/// Third-party agent CLIs are deliberately absent: users install and update
/// them independently, while Core only probes commands already present on the
/// host. Keeping this contract Node-only makes that boundary machine-verifiable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManagedResourcesContract {
    pub schema_version: u8,
    pub runtime_key: String,
    pub node: ManagedNodeResourceContract,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManagedNodeResourceContract {
    pub version: String,
    pub root: String,
    pub executable: String,
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ManagedResourcesContractError {
    message: String,
}

impl ManagedResourcesContractError {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    fn io(action: &str, path: &Path, error: std::io::Error) -> Self {
        Self::invalid(format!("{action} {}: {error}", path.display()))
    }
}

pub fn validate_contract(
    root: &Path,
    contract: &ManagedResourcesContract,
) -> Result<(), ManagedResourcesContractError> {
    if contract.schema_version != MANAGED_RESOURCES_CONTRACT_SCHEMA_VERSION {
        return Err(ManagedResourcesContractError::invalid(format!(
            "unsupported schemaVersion {}",
            contract.schema_version
        )));
    }
    require_non_empty("runtimeKey", &contract.runtime_key)?;
    if !SUPPORTED_RUNTIME_KEYS.contains(&contract.runtime_key.as_str()) {
        return Err(ManagedResourcesContractError::invalid(format!(
            "unsupported runtimeKey {}",
            contract.runtime_key
        )));
    }
    validate_node_schema(&contract.node)?;
    validate_node_paths(root, &contract.node)
}

pub fn write_contract(
    root: &Path,
    contract: &ManagedResourcesContract,
) -> Result<PathBuf, ManagedResourcesContractError> {
    validate_contract(root, contract)?;
    let path = root.join(MANAGED_RESOURCES_CONTRACT_FILE);
    let mut contents = serde_json::to_string_pretty(contract).map_err(|error| {
        ManagedResourcesContractError::invalid(format!("serialize managed resources contract: {error}"))
    })?;
    contents.push('\n');
    fs::write(&path, contents).map_err(|error| ManagedResourcesContractError::io("write contract", &path, error))?;
    Ok(path)
}

pub fn relative_contract_path(base: &Path, path: &Path) -> Result<String, ManagedResourcesContractError> {
    let relative = path.strip_prefix(base).map_err(|_| {
        ManagedResourcesContractError::invalid(format!(
            "path {} is not under managed resources root {}",
            path.display(),
            base.display()
        ))
    })?;
    let value = relative.to_string_lossy().replace('\\', "/");
    validate_contract_relative_path(&value)?;
    Ok(value)
}

fn validate_node_schema(node: &ManagedNodeResourceContract) -> Result<(), ManagedResourcesContractError> {
    require_non_empty("node.version", &node.version)?;
    validate_contract_relative_path_field("node.root", &node.root)?;
    validate_contract_relative_path_field("node.executable", &node.executable)
}

fn validate_node_paths(root: &Path, node: &ManagedNodeResourceContract) -> Result<(), ManagedResourcesContractError> {
    let node_root = root.join(&node.root);
    if !node_root.is_dir() {
        return Err(ManagedResourcesContractError::invalid(format!(
            "required directory missing: {}",
            node_root.display()
        )));
    }
    let executable = node_root.join(&node.executable);
    if !executable.is_file() {
        return Err(ManagedResourcesContractError::invalid(format!(
            "required file missing: {}",
            executable.display()
        )));
    }
    Ok(())
}

fn require_non_empty(field: impl std::fmt::Display, value: &str) -> Result<(), ManagedResourcesContractError> {
    if value.is_empty() {
        return Err(ManagedResourcesContractError::invalid(format!("{field} is required")));
    }
    Ok(())
}

fn validate_contract_relative_path_field(
    field: impl std::fmt::Display,
    value: &str,
) -> Result<(), ManagedResourcesContractError> {
    validate_contract_relative_path(value)
        .map_err(|error| ManagedResourcesContractError::invalid(format!("{field}: {error}")))
}

fn validate_contract_relative_path(value: &str) -> Result<(), ManagedResourcesContractError> {
    if value.is_empty()
        || value.contains('\\')
        || Path::new(value).is_absolute()
        || Path::new(value)
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ManagedResourcesContractError::invalid(format!(
            "invalid relative contract path {value:?}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example_contract(runtime_key: &str) -> ManagedResourcesContract {
        ManagedResourcesContract {
            schema_version: MANAGED_RESOURCES_CONTRACT_SCHEMA_VERSION,
            runtime_key: runtime_key.into(),
            node: ManagedNodeResourceContract {
                version: "24.11.0".into(),
                root: "node/node-v24.11.0-win-x64".into(),
                executable: "node.exe".into(),
            },
        }
    }

    #[test]
    fn contract_serializes_node_only_v3_schema() {
        let value = serde_json::to_value(example_contract("win32-x64")).expect("serialize");
        assert_eq!(value["schemaVersion"], 3);
        assert_eq!(value["runtimeKey"], "win32-x64");
        assert!(value.get("clis").is_none());
    }

    #[test]
    fn contract_deserialization_rejects_third_party_cli_payloads() {
        let value = serde_json::json!({
            "schemaVersion": 3,
            "runtimeKey": "win32-x64",
            "node": {
                "version": "24.11.0",
                "root": "node/node-v24.11.0-win-x64",
                "executable": "node.exe"
            },
            "clis": [{"name": "codex"}]
        });
        assert!(serde_json::from_value::<ManagedResourcesContract>(value).is_err());
    }

    #[test]
    fn validate_contract_rejects_unsafe_node_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        for bad in ["/abs/path", "node\\runtime", "", "../escape", "node/../escape"] {
            let mut contract = example_contract("win32-x64");
            contract.node.root = bad.into();
            let error = validate_contract(temp.path(), &contract).expect_err("unsafe path should fail");
            assert!(error.to_string().contains("invalid relative contract path"), "{error}");
        }
    }

    #[test]
    fn validate_contract_rejects_missing_node_runtime() {
        let temp = tempfile::tempdir().expect("tempdir");
        let error =
            validate_contract(temp.path(), &example_contract("win32-x64")).expect_err("missing node should fail");
        assert!(error.to_string().contains("required directory missing"));
    }
}
