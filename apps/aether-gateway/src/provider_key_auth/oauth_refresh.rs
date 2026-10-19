//! Shared automatic-refresh eligibility and safe diagnostics for the account pool.
use aether_data_contracts::repository::provider_catalog::StoredProviderCatalogKey;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use super::{provider_key_auth_semantics, provider_key_can_refresh_oauth};

pub(crate) const OAUTH_REFRESH_RETRY_DELAY_SECS: u64 = 300;

pub(crate) fn oauth_refresh_success_metadata(
    auth_config: Option<&Map<String, Value>>,
    now: u64,
) -> Value {
    json!({
        "credential_fingerprint": refresh_token_fingerprint(auth_config),
        "state": "succeeded",
        "last_attempt_at": now,
        "next_retry_at": null,
        "error_code": null,
    })
}

fn refresh_token_fingerprint(config: Option<&Map<String, Value>>) -> Option<String> {
    let token = config.and_then(|config| {
        ["refresh_token", "refreshToken"].iter().find_map(|field| {
            config
                .get(*field)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
    })?;
    Some(format!("{:x}", Sha256::digest(token.as_bytes())))
}

fn terminal_refresh_code(reason: &str) -> Option<&'static str> {
    let reason = reason.to_ascii_lowercase();
    [
        "refresh_token_invalidated",
        "refresh_token_reused",
        "refresh_token_expired",
        "invalid_refresh_token",
    ]
    .into_iter()
    .find(|code| reason.contains(code))
    .or_else(|| {
        reason
            .contains("refresh_token 已被使用并轮换")
            .then_some("refresh_token_reused")
    })
    .or_else(|| {
        reason
            .contains("refresh_token 无效、已过期或已撤销")
            .then_some("invalid_refresh_token")
    })
}

/// Never expose the upstream response or credentials in this record.
pub(crate) fn oauth_refresh_failure_metadata(
    auth_config: Option<&Map<String, Value>>,
    status_code: u16,
    normalized_reason: &str,
    now: u64,
) -> Value {
    let terminal_code = terminal_refresh_code(normalized_reason);
    let retryable = terminal_code.is_none()
        && (status_code == 0 || status_code == 408 || status_code == 429 || status_code >= 500);
    json!({
        "credential_fingerprint": refresh_token_fingerprint(auth_config),
        "state": if terminal_code.is_some() { "invalid" } else if retryable { "retry_wait" } else { "failed" },
        "error_code": terminal_code,
        "http_status": status_code,
        "last_attempt_at": now,
        "next_retry_at": retryable.then(|| now.saturating_add(OAUTH_REFRESH_RETRY_DELAY_SECS)),
    })
}

/// Eligibility is independent of the next expiry time and of the global task switch.
/// A healthy credential with an RT remains eligible while waiting for its scheduled refresh.
pub(crate) fn provider_key_oauth_refresh_status(
    key: &StoredProviderCatalogKey,
    provider_type: &str,
    auth_config: Option<&Map<String, Value>>,
    now: u64,
) -> Value {
    let semantics = provider_key_auth_semantics(key, provider_type);
    let reason = key.oauth_invalid_reason.as_deref().unwrap_or_default();
    let at_rejected = reason.starts_with("[OAUTH_EXPIRED]");
    let at_state = if at_rejected {
        "rejected"
    } else if key
        .expires_at_unix_secs
        .is_some_and(|expires| expires <= now)
    {
        "expired"
    } else if key.expires_at_unix_secs.is_some() {
        "valid"
    } else {
        "unknown"
    };
    let record = key
        .upstream_metadata
        .as_ref()
        .and_then(|value| value.get("oauth_refresh"))
        .filter(|record| {
            record
                .get("credential_fingerprint")
                .and_then(Value::as_str)
                .is_some_and(|fingerprint| {
                    Some(fingerprint.to_string()) == refresh_token_fingerprint(auth_config)
                })
        });
    // Older deployments stored only a reason. Quota/Cloudflare 403 errors also use
    // REFRESH_FAILED, so require the actual token-endpoint failure prefix here.
    let has_record = key
        .upstream_metadata
        .as_ref()
        .and_then(|value| value.get("oauth_refresh"))
        .is_some();
    let legacy_failure = (!has_record)
        .then(|| {
            reason
                .lines()
                .find(|line| line.trim().starts_with("[REFRESH_FAILED] Token 续期失败"))
        })
        .flatten();
    let legacy_terminal = legacy_failure.and_then(terminal_refresh_code);
    let rt_state = record
        .and_then(|record| record.get("state"))
        .and_then(Value::as_str)
        .unwrap_or(if legacy_terminal.is_some() {
            "invalid"
        } else if legacy_failure.is_some() {
            "failed"
        } else {
            "unverified"
        });
    let account = aether_admin::provider::status::resolve_account_status_snapshot(
        Some(provider_type),
        key.upstream_metadata.as_ref(),
        Some(reason),
    );
    let account_blocked = account.blocked
        && !matches!(
            account.code.as_str(),
            "oauth_token_invalid"
                | "oauth_token_expired"
                | "oauth_expired"
                | "oauth_refresh_failed"
        );
    let supported = provider_key_can_refresh_oauth(semantics, provider_type, auth_config);
    let (eligible, explanation) = if !semantics.oauth_managed() {
        (false, "当前账号不是 OAuth 凭据")
    } else if !supported {
        (false, "未配置可用于刷新 AT 的 RT，或提供商不支持自动刷新")
    } else if account_blocked {
        (false, "账号访问受限，停止自动刷新")
    } else if rt_state == "invalid" {
        (
            false,
            "RT 已明确失效，停止自动重试；需要重新登录或导入新的 RT",
        )
    } else if rt_state == "failed" {
        (
            false,
            "RT 刷新失败，已停止自动重试；尚不能确定 RT 本身失效，请检查授权配置或手动重试",
        )
    } else if !key.is_active {
        (false, "账号已停用，不参与自动刷新")
    } else if rt_state == "retry_wait" {
        (
            true,
            "RT 刷新遇到临时故障，5 分钟后自动重试；尚不能确定 RT 失效",
        )
    } else if at_rejected {
        (
            true,
            "当前 AT 失效后尚未尝试 RT 恢复；下一轮自动刷新将尝试一次",
        )
    } else if rt_state == "succeeded" {
        (
            true,
            "最近一次 RT 刷新成功，已更新 AT；等待下次到期前自动刷新",
        )
    } else {
        (true, "已配置 RT，可自动刷新；RT 是否有效需实际刷新验证")
    };
    json!({
        "eligible": eligible,
        "reason": explanation,
        "at_state": at_state,
        "rt_state": if !supported { "unavailable" } else { rt_state },
        "last_attempt_at": record.and_then(|record| record.get("last_attempt_at")).and_then(Value::as_u64),
        "next_retry_at": record.and_then(|record| record.get("next_retry_at")).and_then(Value::as_u64),
        "error_code": record.and_then(|record| record.get("error_code")).and_then(Value::as_str).and_then(terminal_refresh_code).or(legacy_terminal),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> StoredProviderCatalogKey {
        let mut key = StoredProviderCatalogKey::new(
            "key".into(),
            "provider".into(),
            "account".into(),
            "oauth".into(),
            None,
            true,
        )
        .unwrap();
        key.encrypted_auth_config = Some("encrypted".into());
        key.expires_at_unix_secs = Some(9999);
        key
    }

    #[test]
    fn first_at_rejection_is_recoverable_even_before_saved_expiry() {
        let mut key = key();
        key.oauth_invalid_at_unix_secs = Some(100);
        key.oauth_invalid_reason = Some("[OAUTH_EXPIRED] ChatGPT Web Token 无效或已过期".into());
        let config = json!({"refresh_token": "test-rt"});
        let status =
            provider_key_oauth_refresh_status(&key, "chatgpt_web", config.as_object(), 100);
        assert_eq!(status["eligible"], true);
        assert_eq!(status["at_state"], "rejected");
        assert_eq!(status["rt_state"], "unverified");
    }

    #[test]
    fn legacy_invalidated_stops_retries_but_quota_403_does_not_invalidate_rt() {
        let mut key = key();
        let config = json!({"refresh_token": "test-rt"});
        key.oauth_invalid_reason = Some("[OAUTH_EXPIRED] AT rejected\n[REFRESH_FAILED] Token 续期失败 (401): HTTP 401 (upstream_error_code=refresh_token_invalidated)".into());
        let status =
            provider_key_oauth_refresh_status(&key, "chatgpt_web", config.as_object(), 100);
        assert_eq!(status["eligible"], false);
        assert_eq!(status["rt_state"], "invalid");
        key.oauth_invalid_reason = Some("[REFRESH_FAILED] ChatGPT Web 访问验证失败".into());
        assert_eq!(
            provider_key_oauth_refresh_status(&key, "chatgpt_web", config.as_object(), 100)
                ["rt_state"],
            "unverified"
        );
    }

    #[test]
    fn transient_failure_backs_off_and_new_rt_does_not_inherit_failure() {
        let mut key = key();
        let config = json!({"refreshToken": "test-rt"});
        let record = oauth_refresh_failure_metadata(config.as_object(), 503, "HTTP 503", 100);
        key.upstream_metadata = Some(json!({"oauth_refresh": record}));
        let status =
            provider_key_oauth_refresh_status(&key, "chatgpt_web", config.as_object(), 110);
        assert_eq!(status["eligible"], true);
        assert_eq!(status["next_retry_at"], 400);
        assert_eq!(status["rt_state"], "retry_wait");
        assert!(!status.to_string().contains("test-rt"));
        let new_config = json!({"refresh_token": "replacement-rt"});
        assert_eq!(
            provider_key_oauth_refresh_status(&key, "chatgpt_web", new_config.as_object(), 110)
                ["rt_state"],
            "unverified"
        );
        assert_eq!(
            provider_key_oauth_refresh_status(&key, "chatgpt_web", None, 110)["eligible"],
            false
        );
    }

    #[test]
    fn rejected_refresh_stops_automatic_attempts_without_claiming_rt_is_invalid() {
        let mut key = key();
        let config = json!({"refresh_token": "test-rt"});
        let record = oauth_refresh_failure_metadata(config.as_object(), 401, "HTTP 401", 100);
        key.upstream_metadata = Some(json!({"oauth_refresh": record}));
        let status =
            provider_key_oauth_refresh_status(&key, "chatgpt_web", config.as_object(), 200);
        assert_eq!(status["eligible"], false);
        assert_eq!(status["rt_state"], "failed");
        assert_eq!(status["error_code"], Value::Null);
    }

    #[test]
    fn replacing_invalid_rt_allows_recovery_even_if_old_reason_remains() {
        let mut key = key();
        let config = json!({"refresh_token": "old-rt"});
        key.upstream_metadata = Some(
            json!({"oauth_refresh": oauth_refresh_failure_metadata(config.as_object(), 401, "refresh_token_invalidated", 100)}),
        );
        key.oauth_invalid_reason = Some("[OAUTH_EXPIRED] AT rejected\n[REFRESH_FAILED] Token 续期失败 (401): refresh_token_invalidated".into());
        assert_eq!(
            provider_key_oauth_refresh_status(&key, "chatgpt_web", config.as_object(), 200)
                ["eligible"],
            false
        );
        let new_config = json!({"refresh_token": "new-rt"});
        assert_eq!(
            provider_key_oauth_refresh_status(&key, "chatgpt_web", new_config.as_object(), 200)
                ["eligible"],
            true
        );
    }
}
