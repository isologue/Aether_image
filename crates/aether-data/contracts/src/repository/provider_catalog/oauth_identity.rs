//! Candidate indexes preserve provider-specific admission rules in the gateway.
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// Held across the authoritative duplicate read and the committed credential write.
/// PostgreSQL implementations release their transaction lock on drop, including cancellation.
pub trait ProviderOAuthIdentityLock: Send {}

/// These hashes select a superset of possible matches; they are not unique constraints.
/// The original provider-specific matcher remains authoritative.
pub fn provider_oauth_identity_hashes(auth: &Map<String, Value>) -> Vec<String> {
    let fields: &[(&str, &[&str])] = &[
        ("email", &["email"]),
        ("user_id", &["user_id"]),
        ("account_id", &["account_id"]),
        ("account_uuid", &["account_uuid", "accountUuid"]),
        ("agent_runtime_id", &["agent_runtime_id", "agentRuntimeId"]),
        ("account_user_id", &["account_user_id"]),
        ("credential_fingerprint", &["credential_fingerprint"]),
    ];
    let mut hashes = Vec::new();
    for (kind, aliases) in fields {
        for alias in *aliases {
            let Some(value) = auth
                .get(*alias)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            let mut digest = Sha256::new();
            digest.update(b"aether-oauth-identity-v1\0");
            digest.update(kind.as_bytes());
            digest.update([0]);
            digest.update(value.to_ascii_lowercase().as_bytes());
            hashes.push(format!("{:x}", digest.finalize()));
        }
    }
    hashes.sort_unstable();
    hashes.dedup();
    hashes
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn identity_aliases_and_case_select_the_same_candidates_without_indexing_tokens() {
        let a = json!({"account_uuid":" abc ","email":"USER@EXAMPLE.COM","access_token":"secret"});
        let b = json!({"accountUuid":"abc","email":"user@example.com","access_token":"different"});
        assert_eq!(
            provider_oauth_identity_hashes(a.as_object().unwrap()),
            provider_oauth_identity_hashes(b.as_object().unwrap())
        );
        assert!(provider_oauth_identity_hashes(
            json!({"access_token":"secret"}).as_object().unwrap()
        )
        .is_empty());
        assert_ne!(
            provider_oauth_identity_hashes(json!({"email":"abc"}).as_object().unwrap()),
            provider_oauth_identity_hashes(json!({"user_id":"abc"}).as_object().unwrap())
        );
    }
}
