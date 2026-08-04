use sha2::{Digest, Sha256};

const SNAPSHOT_DOMAIN: &[u8] = b"tjuae-runtime-asset-snapshot-v2\0";

/// Borrowed, persistence-safe input to the shared runtime asset snapshot
/// digest. Callers must validate their domain values before hashing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeAssetDigestInput<'a> {
    pub local_asset_id: &'a str,
    pub kind: &'a str,
    pub local_definition_digest: &'a str,
    pub runtime_content_digest: &'a str,
    pub upstream_package: Option<&'a str>,
    pub upstream_asset_id: Option<&'a str>,
    pub upstream_version: Option<&'a str>,
    pub upstream_revision: Option<&'a str>,
}

/// Compute the canonical runtime snapshot ID shared by runtime verification
/// and trace persistence. Ordering is deliberately independent of caller
/// input order.
pub fn compute_runtime_asset_snapshot_id(mut assets: Vec<RuntimeAssetDigestInput<'_>>) -> String {
    assets.sort_by(|left, right| (left.kind, left.local_asset_id).cmp(&(right.kind, right.local_asset_id)));
    let mut hasher = Sha256::new();
    hasher.update(SNAPSHOT_DOMAIN);
    for asset in assets {
        hash_length_prefixed(&mut hasher, asset.kind.as_bytes());
        hash_length_prefixed(&mut hasher, asset.local_asset_id.as_bytes());
        hash_length_prefixed(&mut hasher, asset.local_definition_digest.as_bytes());
        hash_length_prefixed(&mut hasher, asset.runtime_content_digest.as_bytes());
        hash_optional(&mut hasher, asset.upstream_package);
        hash_optional(&mut hasher, asset.upstream_asset_id);
        hash_optional(&mut hasher, asset.upstream_version);
        hash_optional(&mut hasher, asset.upstream_revision);
    }
    format!("sha256-{:x}", hasher.finalize())
}

fn hash_length_prefixed(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value);
}

fn hash_optional(hasher: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hash_length_prefixed(hasher, value.as_bytes());
        }
        None => hasher.update([0]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_is_order_independent_and_sensitive_to_upstream_identity() {
        let first = RuntimeAssetDigestInput {
            local_asset_id: "a",
            kind: "assistant",
            local_definition_digest: "sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            runtime_content_digest: "sha256-cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            upstream_package: None,
            upstream_asset_id: None,
            upstream_version: None,
            upstream_revision: None,
        };
        let second = RuntimeAssetDigestInput {
            local_asset_id: "b",
            kind: "skill",
            local_definition_digest: "sha256-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            runtime_content_digest: "sha256-dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
            upstream_package: Some("tjuae/assets"),
            upstream_asset_id: Some("b"),
            upstream_version: Some("1.0.0"),
            upstream_revision: Some("revision"),
        };

        assert_eq!(
            compute_runtime_asset_snapshot_id(vec![first, second]),
            compute_runtime_asset_snapshot_id(vec![second, first])
        );
        assert_ne!(
            compute_runtime_asset_snapshot_id(vec![second]),
            compute_runtime_asset_snapshot_id(vec![RuntimeAssetDigestInput {
                upstream_version: Some("1.0.1"),
                ..second
            }])
        );
    }
}
