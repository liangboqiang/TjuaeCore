use sha2::{Digest, Sha256};
use tjuaeui_api_types::AssetKind;

use crate::AssetError;

const PROJECTION_ID_DOMAIN: &[u8] = b"tjuae-runtime-projection-id/v1";
pub const PROJECTION_RUNTIME_ID_PREFIX: &str = "tjuae-proj-v1-";
pub const PROJECTION_RUNTIME_ID_LENGTH: usize = PROJECTION_RUNTIME_ID_PREFIX.len() + 64;

/// Derive the stable, globally unique identity used only by Core's legacy
/// projection tables and directories.
///
/// The portable Definition `runtimeId` is intentionally not an input: changing
/// a display/runtime alias must not move ownership to another user's projection,
/// while the complete user/owner/local/kind tuple prevents cross-user collision.
pub fn derive_projection_runtime_id(
    user_id: &str,
    asset_owner_id: &str,
    local_asset_id: &str,
    kind: AssetKind,
) -> Result<String, AssetError> {
    for (label, value) in [
        ("userId", user_id),
        ("assetOwnerId", asset_owner_id),
        ("localAssetId", local_asset_id),
    ] {
        if value.trim().is_empty() || value.contains('\0') {
            return Err(AssetError::InvalidMetadata(format!("投影身份字段 {label} 无效")));
        }
    }

    let mut digest = Sha256::new();
    digest.update(PROJECTION_ID_DOMAIN);
    for value in [user_id, asset_owner_id, local_asset_id, kind_name(kind)] {
        digest.update((value.len() as u64).to_be_bytes());
        digest.update(value.as_bytes());
    }
    Ok(format!(
        "{PROJECTION_RUNTIME_ID_PREFIX}{}",
        hex::encode(digest.finalize())
    ))
}

pub fn is_projection_runtime_id(value: &str) -> bool {
    value.len() == PROJECTION_RUNTIME_ID_LENGTH
        && value.starts_with(PROJECTION_RUNTIME_ID_PREFIX)
        && value[PROJECTION_RUNTIME_ID_PREFIX.len()..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn kind_name(kind: AssetKind) -> &'static str {
    match kind {
        AssetKind::Assistant => "assistant",
        AssetKind::EngineAdapter => "engineAdapter",
        AssetKind::Skill => "skill",
        AssetKind::Mcp => "mcp",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_id_is_stable_and_safe() {
        let first = derive_projection_runtime_id("user-a", "owner-a", "asset-a", AssetKind::Skill).unwrap();
        let second = derive_projection_runtime_id("user-a", "owner-a", "asset-a", AssetKind::Skill).unwrap();
        assert_eq!(first, second);
        assert!(is_projection_runtime_id(&first));
        assert_eq!(first.len(), PROJECTION_RUNTIME_ID_LENGTH);
    }

    #[test]
    fn every_ownership_dimension_changes_the_projection_id() {
        let baseline = derive_projection_runtime_id("user-a", "owner-a", "asset-a", AssetKind::Skill).unwrap();
        for candidate in [
            derive_projection_runtime_id("user-b", "owner-a", "asset-a", AssetKind::Skill).unwrap(),
            derive_projection_runtime_id("user-a", "owner-b", "asset-a", AssetKind::Skill).unwrap(),
            derive_projection_runtime_id("user-a", "owner-a", "asset-b", AssetKind::Skill).unwrap(),
            derive_projection_runtime_id("user-a", "owner-a", "asset-a", AssetKind::Assistant).unwrap(),
        ] {
            assert_ne!(baseline, candidate);
        }
    }

    #[test]
    fn length_prefixing_prevents_tuple_boundary_ambiguity() {
        let left = derive_projection_runtime_id("ab", "c", "asset", AssetKind::Mcp).unwrap();
        let right = derive_projection_runtime_id("a", "bc", "asset", AssetKind::Mcp).unwrap();
        assert_ne!(left, right);
    }
}
