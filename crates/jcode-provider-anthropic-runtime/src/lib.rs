//! Direct Anthropic Messages API provider runtime (OAuth subscription + API
//! key, SSE streaming, service tiers, reasoning efforts), moved out of
//! `jcode-base` so provider edits compile only this crate plus a binary
//! relink instead of rebuilding the base -> app-core -> tui spine. The
//! binary's composition root registers [`AnthropicProvider`] with
//! `jcode_base::provider::external` at startup.
//!
//! Pure header/attribution helpers that base's usage/sidecar code needs
//! (`apply_oauth_attribution_headers`, `CLAUDE_CLI_USER_AGENT`, API-key
//! loading, cache-TTL toggles) stay in `jcode_base::provider::anthropic`;
//! this crate re-uses them from there.

//! Direct Anthropic API provider
//!
//! Uses the Anthropic Messages API directly without the Python SDK.
//! This provides better control and eliminates the Python dependency.

use jcode_base::auth;
use jcode_base::auth::oauth;
use jcode_provider_core::{EventStream, NativeToolResultSender, Provider};
fn oauth_beta_headers(model: &str) -> &'static str {
    jcode_provider_core::anthropic_oauth_beta_headers(model)
}

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::StreamExt;
use jcode_base::provider::anthropic::{
    AVAILABLE_MODELS, AnthropicCredentialMode, CLAUDE_CLI_USER_AGENT,
    apply_oauth_attribution_headers, is_cache_ttl_1h, load_anthropic_api_key,
};
#[cfg(test)]
use jcode_base::provider::anthropic::{OAUTH_BETA_HEADERS, effectively_1m};
#[cfg(test)]
use jcode_message_types::{ContentBlock, Role};
use jcode_message_types::{Message, StreamEvent, ToolDefinition};
#[cfg(test)]
use jcode_provider_anthropic::{ApiContentBlock, ToolResultContent, ToolResultContentBlock};
use jcode_provider_anthropic::{
    ApiMessage, ApiMetadata, ApiOutputConfig, ApiRequest, ApiSystem, ApiThinking, ApiTool,
};
use jcode_provider_core::{
    anthropic_is_1m_model as is_1m_model,
    anthropic_map_tool_name_from_oauth as map_tool_name_from_oauth,
    anthropic_strip_1m_suffix as strip_1m_suffix,
};
use reqwest::Client;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::Serialize;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{RwLock, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use uuid::Uuid;

/// Anthropic Messages API endpoint
const API_URL: &str = "https://api.anthropic.com/v1/messages";

/// OAuth endpoint (with beta=true query param)
const API_URL_OAUTH: &str = "https://api.anthropic.com/v1/messages?beta=true";

fn direct_api_url() -> String {
    let base = std::env::var("JCODE_ANTHROPIC_API_BASE")
        .ok()
        .or_else(|| std::env::var("ANTHROPIC_BASE_URL").ok())
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty());
    match base {
        Some(base) if base.ends_with("/messages") => base,
        Some(base) => format!("{base}/messages"),
        None => API_URL.to_string(),
    }
}

fn configured_direct_headers() -> Result<HeaderMap> {
    let Some(raw) = std::env::var("JCODE_ANTHROPIC_HEADERS")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(HeaderMap::new());
    };
    let headers: std::collections::BTreeMap<String, String> = serde_json::from_str(&raw)
        .context("JCODE_ANTHROPIC_HEADERS must be a JSON object of string values")?;
    let mut result = HeaderMap::new();
    for (name, value) in headers {
        let name = HeaderName::from_bytes(name.as_bytes())
            .with_context(|| format!("invalid Anthropic-compatible header name '{name}'"))?;
        let value = HeaderValue::from_str(&value)
            .with_context(|| format!("invalid value for Anthropic-compatible header '{name}'"))?;
        result.insert(name, value);
    }
    Ok(result)
}

fn direct_auth_mode() -> String {
    std::env::var("JCODE_ANTHROPIC_AUTH")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| {
            if std::env::var("ANTHROPIC_AUTH_TOKEN")
                .ok()
                .is_some_and(|value| !value.trim().is_empty())
            {
                "bearer".to_string()
            } else {
                "header".to_string()
            }
        })
        .trim()
        .to_ascii_lowercase()
}

#[derive(Clone)]
struct DirectTransportConfig {
    api_url: String,
    headers: std::result::Result<HeaderMap, String>,
    auth_mode: String,
    auth_header: String,
}

impl DirectTransportConfig {
    fn from_env() -> Self {
        Self {
            api_url: direct_api_url(),
            headers: configured_direct_headers().map_err(|err| format!("{err:#}")),
            auth_mode: direct_auth_mode(),
            auth_header: std::env::var("JCODE_ANTHROPIC_AUTH_HEADER")
                .unwrap_or_else(|_| "x-api-key".to_string()),
        }
    }
}

fn active_anthropic_profile_models() -> Option<Vec<String>> {
    let Ok(profile_name) = std::env::var("JCODE_NAMED_PROVIDER_PROFILE") else {
        return None;
    };
    let Some(profile) = jcode_base::config::config().providers.get(&profile_name) else {
        return None;
    };
    if !matches!(
        profile.provider_type,
        jcode_base::config::NamedProviderType::AnthropicCompatible
    ) {
        return None;
    }
    let mut models = profile
        .models
        .iter()
        .map(|configured| configured.id.clone())
        .collect::<Vec<_>>();
    if let Some(default) = &profile.default_model
        && !models.contains(default)
    {
        models.push(default.clone());
    }
    Some(models)
}

/// Test-only seam so `stream_response` can be pointed at a local mock HTTP
/// server instead of the real Anthropic endpoint. `API_URL`/`API_URL_OAUTH`
/// are `const`, so this is the smallest indirection that lets a test drive
/// the real retry/rotation loop end-to-end. Compiled out entirely in
/// non-test builds (no `std::env::var` involved, nothing readable in a
/// release binary): `test_override_url` reduces to a constant `None`, leaving
/// the caller's real URL untouched.
#[cfg(test)]
static TEST_API_URL_OVERRIDE: std::sync::OnceLock<std::sync::Mutex<Option<String>>> =
    std::sync::OnceLock::new();

#[cfg(test)]
pub(crate) fn set_test_api_url_override(url: Option<String>) {
    *TEST_API_URL_OVERRIDE
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = url;
}

/// Ceiling on a retry sleep under `cfg(test)`.
///
/// The rate-limit and capacity backoffs are measured in seconds because that is
/// what outlasts a real burst window. A test that drives an always-429 server
/// through the whole budget would therefore really sleep for minutes, which is
/// how a 5s suite became a 259s one. Production keeps the real clock; tests
/// exercise the same branch and counters at a millisecond scale.
#[cfg(test)]
const TEST_MAX_RETRY_DELAY_MS: u64 = 5;

/// Apply the test-only sleep ceiling. Identity in non-test builds.
fn clamp_retry_delay(delay: std::time::Duration) -> std::time::Duration {
    #[cfg(test)]
    {
        return delay.min(std::time::Duration::from_millis(TEST_MAX_RETRY_DELAY_MS));
    }
    #[cfg(not(test))]
    delay
}

/// Returns the test-only endpoint override when one is installed, else `None`
/// so the caller keeps whatever real URL it resolved. Compiled out entirely in
/// non-test builds, where it is a constant `None` and the caller's URL stands.
fn test_override_url() -> Option<String> {
    #[cfg(test)]
    {
        return TEST_API_URL_OVERRIDE.get().and_then(|cell| {
            cell.lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        });
    }
    #[cfg(not(test))]
    None
}

#[cfg(test)]
pub(crate) const OAUTH_BETA_HEADERS_1M: &str = jcode_provider_core::ANTHROPIC_OAUTH_BETA_HEADERS_1M;

#[derive(Debug, Clone, Default)]
struct OAuthClientMetadata {
    device_id: Option<String>,
    account_uuid: Option<String>,
    organization_uuid: Option<String>,
    email_address: Option<String>,
}

fn load_official_claude_client_metadata() -> OAuthClientMetadata {
    let path = match jcode_base::storage::user_home_path(".claude.json") {
        Ok(path) => path,
        Err(_) => return OAuthClientMetadata::default(),
    };
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(_) => return OAuthClientMetadata::default(),
    };
    let parsed: Value = match serde_json::from_str(&content) {
        Ok(parsed) => parsed,
        Err(_) => return OAuthClientMetadata::default(),
    };
    let oauth = parsed.get("oauthAccount");
    OAuthClientMetadata {
        device_id: parsed
            .get("userID")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        account_uuid: oauth
            .and_then(|v| v.get("accountUuid"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        organization_uuid: oauth
            .and_then(|v| v.get("organizationUuid"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        email_address: oauth
            .and_then(|v| v.get("emailAddress"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
    }
}

fn oauth_request_metadata(session_id: &str) -> ApiMetadata {
    let official = load_official_claude_client_metadata();
    let device_id = official.device_id.unwrap_or_else(|| {
        Uuid::new_v5(&Uuid::NAMESPACE_DNS, session_id.as_bytes())
            .simple()
            .to_string()
    });
    let account_uuid = official
        .account_uuid
        .unwrap_or_else(|| "unknown-account".to_string());
    let user_id = json!({
        "device_id": device_id,
        "account_uuid": account_uuid,
        "session_id": session_id,
    })
    .to_string();
    ApiMetadata { user_id }
}

#[derive(Serialize)]
struct OAuthEvalRequest {
    attributes: OAuthEvalAttributes,
    #[serde(rename = "forcedVariations")]
    forced_variations: std::collections::BTreeMap<String, Value>,
    #[serde(rename = "forcedFeatures")]
    forced_features: Vec<String>,
    url: String,
}

#[derive(Serialize)]
struct OAuthEvalAttributes {
    id: String,
    #[serde(rename = "sessionId")]
    session_id: String,
    #[serde(rename = "deviceID")]
    device_id: String,
    platform: String,
    #[serde(rename = "organizationUUID")]
    organization_uuid: String,
    #[serde(rename = "accountUUID")]
    account_uuid: String,
    #[serde(rename = "userType")]
    user_type: String,
    #[serde(rename = "subscriptionType")]
    subscription_type: String,
    #[serde(rename = "rateLimitTier")]
    rate_limit_tier: String,
    #[serde(rename = "firstTokenTime")]
    first_token_time: i64,
    email: String,
    #[serde(rename = "appVersion")]
    app_version: String,
}

async fn oauth_preflight_get(
    client: &Client,
    headers: &reqwest::header::HeaderMap,
    label: &str,
    url: &str,
) -> Result<()> {
    let resp = client
        .get(url)
        .headers(headers.clone())
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = jcode_base::util::http_error_body(resp, "HTTP error").await;
        anyhow::bail!("{} returned {}: {}", label, status, body);
    }

    Ok(())
}

async fn oauth_preflight_post_json<T: Serialize + ?Sized>(
    client: &Client,
    headers: &reqwest::header::HeaderMap,
    label: &str,
    url: &str,
    body: &T,
) -> Result<()> {
    let resp = client
        .post(url)
        .headers(headers.clone())
        .timeout(std::time::Duration::from_secs(5))
        .json(body)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = jcode_base::util::http_error_body(resp, "HTTP error").await;
        anyhow::bail!("{} returned {}: {}", label, status, body);
    }

    Ok(())
}

fn record_oauth_preflight_result(label: &str, result: Result<()>) -> bool {
    match result {
        Ok(()) => true,
        Err(err) => {
            jcode_base::logging::warn(&format!(
                "Claude OAuth preflight {} failed; continuing because Claude Code treats this bootstrap traffic as nonessential: {:#}",
                label, err
            ));
            false
        }
    }
}

async fn ensure_oauth_preflight(
    client: &Client,
    token: &str,
    session_id: &str,
    done_flag: &AtomicBool,
) -> Result<()> {
    if done_flag.load(Ordering::Relaxed) {
        return Ok(());
    }

    let official = load_official_claude_client_metadata();
    let Some(device_id) = official.device_id else {
        jcode_base::logging::warn(
            "Skipping Claude OAuth preflight: missing userID in ~/.claude.json",
        );
        return Ok(());
    };
    let Some(account_uuid) = official.account_uuid else {
        jcode_base::logging::warn(
            "Skipping Claude OAuth preflight: missing accountUuid in ~/.claude.json",
        );
        return Ok(());
    };
    let Some(organization_uuid) = official.organization_uuid else {
        jcode_base::logging::warn(
            "Skipping Claude OAuth preflight: missing organizationUuid in ~/.claude.json",
        );
        return Ok(());
    };
    let Some(email_address) = official.email_address else {
        jcode_base::logging::warn(
            "Skipping Claude OAuth preflight: missing emailAddress in ~/.claude.json",
        );
        return Ok(());
    };

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        reqwest::header::HeaderValue::from_str(&format!("Bearer {}", token))?,
    );
    headers.insert(
        reqwest::header::USER_AGENT,
        reqwest::header::HeaderValue::from_static(CLAUDE_CLI_USER_AGENT),
    );
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    headers.insert(
        reqwest::header::HeaderName::from_static("anthropic-beta"),
        reqwest::header::HeaderValue::from_static("oauth-2025-04-20"),
    );

    let mut all_ok = true;
    all_ok &= record_oauth_preflight_result(
        "bootstrap",
        oauth_preflight_get(
            client,
            &headers,
            "bootstrap",
            "https://api.anthropic.com/api/claude_cli/bootstrap",
        )
        .await,
    );
    all_ok &= record_oauth_preflight_result(
        "account settings",
        oauth_preflight_get(
            client,
            &headers,
            "account settings",
            "https://api.anthropic.com/api/oauth/account/settings",
        )
        .await,
    );
    all_ok &= record_oauth_preflight_result(
        "grove",
        oauth_preflight_get(
            client,
            &headers,
            "grove",
            "https://api.anthropic.com/api/claude_code_grove",
        )
        .await,
    );

    let eval = OAuthEvalRequest {
        attributes: OAuthEvalAttributes {
            id: device_id.clone(),
            session_id: session_id.to_string(),
            device_id: device_id.clone(),
            platform: std::env::consts::OS.to_string(),
            organization_uuid,
            account_uuid,
            user_type: "external".to_string(),
            subscription_type: jcode_base::auth::claude::get_subscription_type()
                .unwrap_or_else(|| "pro".to_string()),
            rate_limit_tier: "default_claude_ai".to_string(),
            first_token_time: 1_740_976_801_491,
            email: email_address,
            app_version: "2.1.123".to_string(),
        },
        forced_variations: Default::default(),
        forced_features: Vec::new(),
        url: String::new(),
    };

    all_ok &= record_oauth_preflight_result(
        "eval",
        oauth_preflight_post_json(
            client,
            &headers,
            "eval",
            "https://api.anthropic.com/api/eval/sdk-zAZezfDKGoZuXXKe",
            &eval,
        )
        .await,
    );

    done_flag.store(true, Ordering::Relaxed);
    if all_ok {
        jcode_base::logging::info("Claude OAuth preflight completed successfully");
    }
    Ok(())
}

/// Quality-first default shared with model routing and first-run selection.
const DEFAULT_MODEL: &str = jcode_provider_core::DEFAULT_CLAUDE_MODEL;

/// API version header
const API_VERSION: &str = "2023-06-01";

/// Maximum number of retries for transient errors
const MAX_RETRIES: u32 = 2;

/// Base delay for exponential backoff (in milliseconds)
const RETRY_BASE_DELAY_MS: u64 = 200;

/// Extra attempts reserved for Anthropic capacity rejections (`overloaded_error`,
/// HTTP 529).
///
/// An overload is not a transport fault: the request was well-formed and the
/// server simply had no capacity right now. It clears on its own, but on the
/// order of seconds, not the ~200ms the transport backoff waits. Sharing the
/// transport budget meant a single overload burned the one available retry
/// almost immediately and surfaced a raw stream error plus a "switch model"
/// prompt, when simply waiting a little longer nearly always succeeds.
const MAX_OVERLOADED_RETRIES: u32 = 4;

/// Base delay for capacity backoff, giving ~2s, 4s, 8s across the retries above.
const OVERLOADED_RETRY_BASE_DELAY_MS: u64 = 2000;

/// Extra attempts reserved for a rate limit that account rotation could not
/// escape, i.e. every account answered 429.
///
/// Anthropic enforces a short-window burst limit *in addition to* the 5-hour and
/// weekly quotas, so "every account is rate limited" routinely means "every
/// account is busy for the next few seconds", not "you are out of quota".
/// Falling through to the transport budget gave such a burst 2 attempts ~200ms
/// apart, which is far too fast to outlast it, so the turn died and offered a
/// model downgrade while the user still had 80%+ of their weekly allowance.
/// Same reasoning as `MAX_OVERLOADED_RETRIES`; see docs/adr/0015.
const MAX_RATE_LIMIT_RETRIES: u32 = 5;

/// Base delay for the rate-limit backoff, giving ~5s, 10s, 20s, 40s. A
/// `retry-after` from the server always wins over this, up to
/// `MAX_BURST_RETRY_AFTER`.
const RATE_LIMIT_RETRY_BASE_DELAY_MS: u64 = 5000;

/// Longest server `retry-after` this loop will honour for a *burst* limit.
///
/// Anthropic answers a spent 5-hour quota and a momentary burst with the same
/// 429, and attaches the same `retry-after: 60`. Honouring that verbatim turned
/// the burst path into four consecutive 60s sleeps: a four-minute silence for a
/// limit that either clears in seconds or does not clear for hours, so waiting a
/// full minute is wrong in both directions. Capping applies only to this path;
/// the quota path no longer waits at all. See ADR-0016.
const MAX_BURST_RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(15);

/// Longest this loop will sleep waiting for the account pool's own cooldown.
///
/// Past this the wait stops being "outlast a burst" and becomes "sit silently
/// through a quota reset", which the user should be told about and allowed to
/// route around instead. See ADR-0023.
const MAX_POOL_RECOVERY_WAIT: std::time::Duration = std::time::Duration::from_secs(90);

/// Widest extra spread added on top of a pool-recovery wait.
///
/// Every session on the daemon computes the *same* deadline from the same shared
/// cooldown, so without this they all wake in the same millisecond and fire a
/// synchronised burst at an account that can serve maybe one of them: a
/// thundering herd that re-limits the pool instantly and starts the cycle again.
///
/// The spread is derived from a per-request salt, so it is stable within a
/// request (a retry does not re-roll into the same slot as a sibling) while
/// still differing between concurrent requests.
const MAX_POOL_RECOVERY_SPREAD_MS: u64 = 2_000;

/// Deterministic spread in `0..MAX_POOL_RECOVERY_SPREAD_MS` for one request.
///
/// `salt` is the request's own identity, so two concurrent requests waiting on
/// the same pool deadline wake at different moments while a single request's
/// retries stay in its own slot.
fn pool_recovery_spread(salt: u64) -> std::time::Duration {
    // Cheap integer hash (splitmix64 finaliser): the salt is a monotonic
    // counter, whose low bits alone would put consecutive requests in adjacent
    // millisecond slots and defeat the point.
    let mut x = salt.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    std::time::Duration::from_millis(x % MAX_POOL_RECOVERY_SPREAD_MS)
}

/// Source of per-request salts for [`pool_recovery_spread`].
static POOL_RECOVERY_SALT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Cached OAuth credentials
#[derive(Clone)]
struct CachedCredentials {
    access_token: String,
    refresh_token: String,
    expires_at: i64,
    /// Anthropic account label this token belongs to, when known.
    ///
    /// Without it, a cached-but-unexpired token is indistinguishable from a
    /// token for the account the shared override now points at, and
    /// `get_oauth_access_token` returns early on expiry alone. One daemon serves
    /// every session (ADR-0009), so when one session rotated the pool to
    /// `claude-1`, every *other* session kept sending its still-valid
    /// `claude-2` token and kept getting 429 from the account that had just
    /// refused - while its own header claimed `claude-1`. See ADR-0024.
    account_label: Option<String>,
}

/// Whether a cached OAuth token may still be used for a request.
///
/// Two independent reasons to refuse it, and only the first used to be checked:
///
/// 1. it is expired (or inside the 5-minute refresh buffer);
/// 2. it belongs to a *different* account than the pool now points at.
///
/// (2) is what made account rotation look like it did nothing outside the
/// session that triggered it. One daemon serves every session (ADR-0009) and the
/// active-account override is process-wide, so a rotation moved the pointer for
/// everyone - but each session's provider was still holding a perfectly valid
/// token for the old account, so it never re-read the pointer and kept sending
/// requests to the account that had just refused them. Nothing rejected the
/// token as auth, so the only symptom was a 429 loop in every session except
/// the one that rotated, with the header claiming the new account.
///
/// An unlabelled token predates this field (a live cache across a reload), so it
/// is refused once and replaced by a labelled one rather than trusted blindly.
/// See ADR-0024.
fn cached_token_is_usable(
    cached: &CachedCredentials,
    active_label: Option<&str>,
    now_ms: i64,
) -> bool {
    if cached.expires_at <= now_ms + 300_000 {
        return false;
    }
    match cached.account_label.as_deref() {
        Some(label) => Some(label) == active_label,
        None => false,
    }
}

/// Direct Anthropic API provider
pub struct AnthropicProvider {
    client: Client,
    model: Arc<std::sync::RwLock<String>>,
    reasoning_effort: Arc<std::sync::RwLock<Option<String>>>,
    service_tier: Arc<std::sync::RwLock<Option<String>>>,
    /// Cached OAuth credentials (None if using API key)
    credentials: Arc<RwLock<Option<CachedCredentials>>>,
    credential_mode: Arc<RwLock<AnthropicCredentialMode>>,
    /// Explicit `JCODE_ANTHROPIC_MAX_TOKENS` override. When unset, the output
    /// budget is derived per model so newer generations are not clamped to the
    /// legacy 32K default.
    max_tokens_override: Option<u32>,
    oauth_session_id: String,
    oauth_preflight_done: Arc<AtomicBool>,
    direct_transport: DirectTransportConfig,
    /// Named profiles pin their credential at runtime construction so another
    /// session/profile cannot redirect this runtime to a different process env.
    profile_api_key: Option<std::result::Result<String, String>>,
    profile_models: Option<Vec<String>>,
}

impl AnthropicProvider {
    /// Whether the active account is blocked by a spent usage window.
    ///
    /// Delegates to `UsageData::exhausted`, a disjunction: either the 5-hour or
    /// the 7-day window being spent blocks the account (ADR-0016). This site
    /// kept a private `&&` copy, so a weekly-only exhaustion read as headroom.
    fn is_usage_exhausted() -> bool {
        jcode_base::usage::get_sync().exhausted()
    }

    fn best_available_opus_model(exclude: &str) -> Option<String> {
        let mut models = jcode_base::provider::cached_anthropic_model_ids()
            .unwrap_or_else(jcode_base::provider::known_anthropic_model_ids);
        models.retain(|model| {
            let key = strip_1m_suffix(model).to_ascii_lowercase();
            key.contains("claude-opus")
                && strip_1m_suffix(model) != strip_1m_suffix(exclude)
                && !anthropic_model_is_retired(model)
        });
        models.sort_by_key(|model| anthropic_model_quality_rank(model));
        models.into_iter().next()
    }

    fn fallback_for_model_scoped_usage(
        selected_model: &str,
        usage: &jcode_base::usage::UsageData,
    ) -> Option<String> {
        (selected_model.to_ascii_lowercase().contains("fable")
            && usage.model_scoped_exhausted(selected_model))
        .then(|| Self::best_available_opus_model(selected_model))
        .flatten()
    }

    async fn model_after_oauth_quota_check(
        &self,
        token: &str,
        is_oauth: bool,
        selected_model: String,
    ) -> String {
        if !is_oauth || !selected_model.to_ascii_lowercase().contains("fable") {
            return selected_model;
        }
        let Ok(usage) = jcode_base::usage::fetch_usage_for_access_token(token).await else {
            return selected_model;
        };
        let Some(fallback) = Self::fallback_for_model_scoped_usage(&selected_model, &usage) else {
            return selected_model;
        };
        jcode_base::logging::warn(&format!(
            "Anthropic OAuth model-scoped weekly quota for '{}' is exhausted; routing to '{}'",
            selected_model, fallback
        ));
        *self
            .model
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = fallback.clone();
        fallback
    }

    /// Resolve a usable access token (OAuth or API key) and whether it is OAuth.
    ///
    /// Exposed for the provider-doctor's native Claude driver so it can validate
    /// the credential and fetch the live model catalog through the exact same
    /// resolution path the runtime uses. Returns the bearer token and an
    /// `is_oauth` flag so callers can pick the matching catalog endpoint.
    pub async fn resolve_access_token_for_doctor(&self) -> Result<(String, bool)> {
        self.get_access_token().await
    }

    /// Pin the credential mode (OAuth vs API key) for a provider-doctor run.
    ///
    /// The `claude` login provider is specifically the OAuth/subscription path,
    /// while `claude-api` is the API-key path. The doctor must test the path
    /// implied by the provider id under test, regardless of what
    /// `JCODE_RUNTIME_PROVIDER` happens to be in the current process (e.g. a
    /// self-dev session may have it set to `claude-api`). This also updates
    /// `JCODE_RUNTIME_PROVIDER` so any provider instances the probes build
    /// afterwards inherit the same mode. Errors if the requested credential is
    /// not available, so the doctor can record a clear AUTH failure.
    pub fn pin_credential_mode_for_doctor(&self, oauth: bool) -> Result<()> {
        let mode = if oauth {
            AnthropicCredentialMode::OAuth
        } else {
            AnthropicCredentialMode::ApiKey
        };
        self.set_credential_mode(mode)
    }

    /// Fetch the live Anthropic model catalog using the resolved credential.
    ///
    /// Mirrors [`Provider::prefetch_models`] but returns the model ids to the
    /// caller (rather than only persisting them) so the doctor can assert the
    /// live `GET /v1/models` endpoint works and that the model under test is in
    /// the live catalog.
    pub async fn fetch_live_model_ids_for_doctor(&self) -> Result<Vec<String>> {
        let (token, is_oauth) = self.get_access_token().await?;
        if token.trim().is_empty() {
            anyhow::bail!("resolved an empty Anthropic access token");
        }
        let catalog = if is_oauth {
            jcode_base::provider::fetch_anthropic_model_catalog_oauth(&token).await?
        } else {
            jcode_base::provider::fetch_anthropic_model_catalog(&token).await?
        };
        // Persist so the rest of the process benefits from the warm catalog,
        // exactly like the runtime's own prefetch.
        jcode_base::provider::persist_anthropic_model_catalog(&catalog);
        if !catalog.context_limits.is_empty() {
            jcode_base::provider::populate_context_limits(catalog.context_limits.clone());
        }
        if !catalog.available_models.is_empty() {
            jcode_base::provider::populate_anthropic_models(catalog.available_models.clone());
        }
        Ok(catalog.available_models)
    }

    pub fn new() -> Self {
        let model = std::env::var("JCODE_ANTHROPIC_MODEL").unwrap_or_else(|_| {
            if Self::is_usage_exhausted() {
                "claude-sonnet-4-6".to_string()
            } else {
                DEFAULT_MODEL.to_string()
            }
        });

        // Trigger background usage fetch so extra_usage is known before first API call
        let _ = tokio::runtime::Handle::try_current().map(|_| {
            tokio::spawn(async {
                let _ = jcode_base::usage::get().await;
            })
        });

        let max_tokens_override = std::env::var("JCODE_ANTHROPIC_MAX_TOKENS")
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok());
        let reasoning_effort = jcode_base::config::config()
            .provider
            .anthropic_reasoning_effort
            .as_deref()
            .and_then(Self::normalize_reasoning_effort)
            .map(|effort| Self::store_effort_for_model(&model, &effort));

        let direct_transport = DirectTransportConfig::from_env();
        let profile_api_key = std::env::var_os("JCODE_ANTHROPIC_API_BASE")
            .map(|_| load_anthropic_api_key().map_err(|err| format!("{err:#}")));
        let profile_models = active_anthropic_profile_models();

        // `/fast default on` is stored once for every provider, under the
        // historical key `openai_service_tier`. Honor it here too: without
        // this an Anthropic session always started standard while the UI
        // reported the saved default as on.
        let service_tier = jcode_base::config::config()
            .provider
            .openai_service_tier
            .as_deref()
            .and_then(|raw| Self::normalize_service_tier(raw).ok().flatten())
            .filter(|_| Self::model_supports_fast_mode(&model));

        Self {
            client: jcode_provider_core::shared_http_client(),
            model: Arc::new(std::sync::RwLock::new(model)),
            reasoning_effort: Arc::new(std::sync::RwLock::new(reasoning_effort)),
            service_tier: Arc::new(std::sync::RwLock::new(service_tier)),
            credentials: Arc::new(RwLock::new(None)),
            credential_mode: Arc::new(RwLock::new(AnthropicCredentialMode::from_runtime_env(
                jcode_provider_core::DualAuthProvider::Anthropic,
            ))),
            max_tokens_override,
            oauth_session_id: Uuid::new_v4().to_string(),
            oauth_preflight_done: Arc::new(AtomicBool::new(false)),
            direct_transport,
            profile_api_key,
            profile_models,
        }
    }

    fn direct_api_key(&self) -> Result<String> {
        match &self.profile_api_key {
            Some(Ok(key)) => Ok(key.clone()),
            Some(Err(err)) => anyhow::bail!(err.clone()),
            None => load_anthropic_api_key(),
        }
    }

    fn normalized_model_key(model: &str) -> String {
        strip_1m_suffix(model).trim().to_ascii_lowercase()
    }

    fn model_supports_output_effort(model: &str) -> bool {
        // Shared capability table (with an optimistic default for unknown 5.x+
        // generations); see `jcode_provider_core::anthropic_reasoning_caps`.
        // Fable 5 verified live 2026-07-01; Sonnet 5 verified live 2026-07-07.
        jcode_provider_core::anthropic_reasoning_caps(model).output_effort
    }

    fn model_supports_adaptive_thinking(model: &str) -> bool {
        jcode_provider_core::anthropic_reasoning_caps(model).adaptive_thinking
    }

    fn model_supports_manual_thinking(model: &str) -> bool {
        jcode_provider_core::anthropic_reasoning_caps(model).manual_thinking
    }

    fn model_supports_xhigh_effort(model: &str) -> bool {
        jcode_provider_core::anthropic_reasoning_caps(model).xhigh_effort
    }

    /// `max` effort ("absolute maximum capability with no constraints on token
    /// spending") is a real API level on the `output_config` effort models,
    /// except Claude Opus 4.5 where manual thinking keeps `max` as an alias for
    /// the strongest supported level.
    fn model_supports_max_effort(model: &str) -> bool {
        jcode_provider_core::anthropic_reasoning_caps(model).max_effort
    }

    fn model_supports_reasoning_effort(model: &str) -> bool {
        jcode_provider_core::anthropic_reasoning_caps(model).supports_reasoning_effort()
    }

    fn normalize_reasoning_effort(raw: &str) -> Option<String> {
        let value = raw.trim().to_ascii_lowercase();
        if value.is_empty() || matches!(value.as_str(), "default" | "auto") {
            return None;
        }
        match value.as_str() {
            "off" | "disabled" => Some("none".to_string()),
            // `swarm` is a UI sentinel meaning "max effort + use the swarm tool".
            // Stored verbatim; resolved to a real effort in `actual_effort_for_model`.
            "none" | "low" | "medium" | "high" | "xhigh" | "max" | "swarm" | "swarm-deep" => {
                Some(value)
            }
            other => {
                jcode_base::logging::info(&format!(
                    "Warning: Ignoring unsupported Anthropic reasoning effort '{}'; expected none|low|medium|high|xhigh|max.",
                    other
                ));
                None
            }
        }
    }

    fn actual_effort_for_model(model: &str, effort: &str) -> String {
        if jcode_base::prompt::is_swarm_effort(effort) {
            // Swarm rungs sit above `max` on the ladder and mean "strongest
            // reasoning the model supports", so cycling upward never lowers
            // the wire effort.
            if Self::model_supports_max_effort(model) {
                "max".to_string()
            } else if Self::model_supports_xhigh_effort(model) {
                "xhigh".to_string()
            } else {
                "high".to_string()
            }
        } else if effort == "max" && !Self::model_supports_max_effort(model) {
            if Self::model_supports_xhigh_effort(model) {
                "xhigh".to_string()
            } else {
                "high".to_string()
            }
        } else if effort == "xhigh" && !Self::model_supports_xhigh_effort(model) {
            "high".to_string()
        } else {
            effort.to_string()
        }
    }

    /// Like [`Self::actual_effort_for_model`], but preserves the swarm sentinels
    /// (light `swarm` and `swarm-deep`) so the stored/UI value keeps reflecting
    /// the chosen swarm mode. Used when persisting the user's choice; request
    /// building resolves swarm to a real effort.
    fn store_effort_for_model(model: &str, effort: &str) -> String {
        if jcode_base::prompt::is_deep_swarm_effort(effort) {
            jcode_base::prompt::SWARM_DEEP_EFFORT.to_string()
        } else if jcode_base::prompt::is_swarm_effort(effort) {
            jcode_base::prompt::SWARM_EFFORT.to_string()
        } else {
            Self::actual_effort_for_model(model, effort)
        }
    }

    /// Default reasoning effort to apply when the user has *not* explicitly
    /// configured one. Claude Opus 5 defaults to `low`: it is strong enough
    /// at low effort for day-to-day coding/agentic work, and users can cycle
    /// up when they want deeper reasoning. Older Claude Opus models are
    /// reasoning-heavy flagships, so we default them to `xhigh` where
    /// supported (Opus 4.7/4.8), clamped to `high` on older Opus.
    /// Deliberately NOT `max`: Anthropic recommends `xhigh` as the starting
    /// point for coding/agentic work and reserves `max` for frontier problems
    /// (it costs much more and can overthink). Claude Fable 5 defaults to
    /// `high`: it benefits from deeper reasoning on coding/agentic work.
    /// Every other model keeps the model's own default (no forced effort) so
    /// cheaper models stay cheap.
    fn default_reasoning_effort_for_model(model: &str) -> Option<String> {
        let key = Self::normalized_model_key(model);
        if key.contains("claude-opus-5") {
            Some("low".to_string())
        } else if key.contains("claude-opus") {
            Some(if Self::model_supports_xhigh_effort(model) {
                "xhigh".to_string()
            } else {
                "high".to_string()
            })
        } else if key.contains("claude-fable-5") {
            // Fable 5 defaults to `high` reasoning for stronger day-to-day
            // results. Users can still cycle down for faster/cheaper turns.
            Some("high".to_string())
        } else {
            None
        }
    }

    /// The raw, user-configured reasoning effort for this provider, if any.
    /// `None` means "use the model default" (see
    /// [`Self::default_reasoning_effort_for_model`]).
    fn stored_reasoning_effort(&self) -> Option<String> {
        self.reasoning_effort
            .read()
            .map(|guard| guard.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone())
    }

    /// Effective reasoning effort for `model`, resolving the model default when
    /// the user has not configured an explicit effort.
    fn effort_for_model(&self, model: &str) -> Option<String> {
        if !Self::model_supports_reasoning_effort(model) {
            return None;
        }
        Some(
            self.stored_reasoning_effort()
                .or_else(|| Self::default_reasoning_effort_for_model(model))
                .unwrap_or_else(|| "none".to_string()),
        )
    }

    fn model_supports_fast_mode(model: &str) -> bool {
        jcode_provider_core::anthropic_supports_fast_mode(model)
    }

    fn normalize_service_tier(raw: &str) -> Result<Option<String>> {
        let value = raw.trim().to_ascii_lowercase();
        match value.as_str() {
            "" | "default" | "off" | "standard" | "standard_only" => Ok(None),
            // `/fast on` is shared with OpenAI, which spells this `priority`.
            // Anthropic's own spelling for fast mode is `speed: "fast"`; the
            // legacy `auto` is accepted so older saved state keeps working.
            "priority" | "auto" | "fast" => Ok(Some("fast".to_string())),
            other => anyhow::bail!(
                "Unsupported Anthropic speed '{}'; expected fast/priority or off/standard",
                other
            ),
        }
    }

    /// `Some("fast")` when fast mode is enabled and `model` supports it.
    ///
    /// Anthropic fast mode is the `speed` request field, NOT `service_tier`.
    /// `service_tier` selects Priority Tier, which is unsupported on Opus 5
    /// and mutually exclusive with fast mode, so this never yields a tier.
    pub(crate) fn fast_speed_for_model(&self, model: &str) -> Option<String> {
        let enabled = self
            .service_tier
            .read()
            .map(|guard| guard.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone())
            .is_some();
        (enabled && Self::model_supports_fast_mode(model)).then(|| "fast".to_string())
    }

    /// Output-token budget for `model`: an explicit env override when set,
    /// otherwise the model's published maximum. A flat default would clamp
    /// 128K-output models to 32K and truncate long agentic turns mid-tool-call.
    fn max_tokens_for(&self, model: &str) -> u32 {
        self.max_tokens_override
            .unwrap_or_else(|| jcode_provider_core::anthropic::anthropic_max_output_tokens(model))
    }

    fn manual_thinking_budget(effort: &str, max_tokens: u32) -> Option<u32> {
        let desired = match effort {
            "low" => 1_024,
            "medium" => 4_096,
            "high" => 8_192,
            "xhigh" | "max" => 16_384,
            e if jcode_base::prompt::is_swarm_effort(e) => 16_384,
            _ => return None,
        };
        let budget = desired.min(max_tokens.saturating_sub(1));
        (budget >= 1_024).then_some(budget)
    }

    fn build_reasoning_request_parts(
        &self,
        model: &str,
        is_oauth: bool,
    ) -> (Option<ApiThinking>, Option<ApiOutputConfig>, Option<f32>) {
        // `display.show_thinking` is a request to *see* the model's reasoning.
        // Anthropic only streams thinking summaries when a thinking request is
        // present, so opting into the display must also opt into generating it.
        let show_thinking = jcode_base::config::config().display.show_thinking;
        self.build_reasoning_request_parts_inner(model, is_oauth, show_thinking)
    }

    fn build_reasoning_request_parts_inner(
        &self,
        model: &str,
        is_oauth: bool,
        show_thinking: bool,
    ) -> (Option<ApiThinking>, Option<ApiOutputConfig>, Option<f32>) {
        let effort = self.effort_for_model(model);
        // An explicit "none" (user-configured or a model default) means
        // reasoning was deliberately disabled, so it must also win over the
        // `display.show_thinking` fallback below. `effort_for_model` returns
        // Some("none") even when nothing is configured, so check the
        // stored/default effort instead.
        let effort_is_explicit_none = self
            .stored_reasoning_effort()
            .or_else(|| Self::default_reasoning_effort_for_model(model))
            .as_deref()
            == Some("none");
        let effort = effort.as_deref().filter(|effort| *effort != "none");
        let show_thinking = show_thinking && !effort_is_explicit_none;

        let output_config = effort
            .filter(|_| Self::model_supports_output_effort(model))
            .map(|effort| ApiOutputConfig {
                effort: Self::actual_effort_for_model(model, effort),
            });

        // When only the display toggle is on (no explicit effort), request
        // thinking without forcing `output_config`, so the model keeps its
        // default reasoning strength and only the thinking *display* is enabled.
        let thinking = if Self::model_supports_adaptive_thinking(model) {
            (effort.is_some() || show_thinking).then_some(ApiThinking::Adaptive {
                display: Some("summarized"),
            })
        } else if Self::model_supports_manual_thinking(model) {
            // Manual-thinking models need a concrete budget. Use the configured
            // effort, or fall back to a minimal budget when only the display
            // toggle is on.
            effort
                .or(show_thinking.then_some("low"))
                .and_then(|effort| Self::manual_thinking_budget(effort, self.max_tokens_for(model)))
                .map(|budget_tokens| ApiThinking::Enabled { budget_tokens })
        } else {
            None
        };

        // Extended/adaptive thinking is incompatible with temperature. OAuth path
        // normally mirrors Claude Code's temperature=1.0, so omit it when thinking is active.
        let temperature = if is_oauth && thinking.is_none() {
            Some(1.0)
        } else {
            None
        };

        (thinking, output_config, temperature)
    }

    /// Get the access token from credentials
    /// Supports both OAuth tokens and direct API keys
    /// Automatically refreshes OAuth tokens when expired
    async fn get_access_token(&self) -> Result<(String, bool)> {
        let mode = *self.credential_mode.read().await;

        // Explicit API-key mode: use the direct API key and surface an error if
        // one is not configured (never silently fall back to OAuth).
        if matches!(mode, AnthropicCredentialMode::ApiKey) {
            let key = self.direct_api_key()?;
            return Ok((key, false)); // false = not OAuth
        }

        // Auto mode prefers OAuth (Claude subscription), but only while those
        // credentials can actually produce a usable access token. A stale
        // jcode OAuth account may still deserialize successfully even after its
        // refresh token has been revoked. Treating that as "OAuth available"
        // prevented the configured API key from ever being tried and made
        // imported Claude Code sessions fail with a 401 on their first turn.
        if matches!(mode, AnthropicCredentialMode::Auto) {
            match self.get_oauth_access_token().await {
                Ok(token) => return Ok(token),
                Err(oauth_err) => match self.direct_api_key() {
                    Ok(key) => {
                        jcode_base::logging::warn(&format!(
                            "Claude OAuth is unusable in automatic credential mode ({oauth_err:#}); falling back to the configured Anthropic API key"
                        ));
                        return Ok((key, false));
                    }
                    Err(_) => return Err(oauth_err),
                },
            }
        }

        self.get_oauth_access_token().await
    }

    async fn get_oauth_access_token(&self) -> Result<(String, bool)> {
        // Which account this session must be on *now*. Re-read per request, not
        // cached, because another session on this daemon may have rotated the
        // shared override since the last turn.
        let active_label = auth::claude::active_account_label();

        // Check cached credentials
        {
            let cached = self.credentials.read().await;
            if let Some(ref creds) = *cached {
                let now = chrono::Utc::now().timestamp_millis();
                if cached_token_is_usable(creds, active_label.as_deref(), now) {
                    return Ok((creds.access_token.clone(), true));
                }
            }
        }

        // Load fresh credentials or refresh expired ones. `load_credentials`
        // already honours the active-account override, so this is the account
        // switch actually taking effect for this session.
        let fresh_creds =
            auth::claude::load_credentials().context("Failed to load Claude credentials")?;

        if !fresh_creds.scopes.is_empty()
            && !oauth::claude_scopes_have_inference(&fresh_creds.scopes)
        {
            anyhow::bail!(
                "Claude OAuth credentials are missing the required user:inference scope (scopes: {}). Run `jcode login --provider claude` to mint a fresh Claude.ai OAuth token, or import/use a fresh Claude Code login.",
                fresh_creds.scopes.join(" ")
            );
        }

        let now = chrono::Utc::now().timestamp_millis();

        // Check if token needs refresh (expired or expiring within 5 minutes)
        if fresh_creds.expires_at < now + 300_000 && !fresh_creds.refresh_token.is_empty() {
            // A refresh token the provider already rejected as unrecoverable
            // cannot start working again. Retrying it added a doomed network
            // round-trip to the critical path of every single turn, so fail
            // straight through to the caller's API-key fallback instead.
            if auth::refresh_state::refresh_token_is_known_rejected(
                "claude",
                &fresh_creds.refresh_token,
            ) {
                anyhow::bail!(
                    "Claude OAuth refresh token was previously rejected by Anthropic and cannot be refreshed. Run `jcode login --provider claude` to mint a fresh token."
                );
            }

            jcode_base::logging::info(
                "OAuth token expired or expiring soon, attempting refresh...",
            );

            let refresh_label = active_label
                .clone()
                .unwrap_or_else(auth::claude::primary_account_label);
            match oauth::refresh_claude_tokens_for_account(
                &fresh_creds.refresh_token,
                &refresh_label,
            )
            .await
            {
                Ok(refreshed) => {
                    jcode_base::logging::info("OAuth token refreshed successfully");

                    // Cache the refreshed credentials
                    let mut cached = self.credentials.write().await;
                    *cached = Some(CachedCredentials {
                        access_token: refreshed.access_token.clone(),
                        refresh_token: refreshed.refresh_token,
                        expires_at: refreshed.expires_at,
                        account_label: Some(refresh_label.clone()),
                    });

                    return Ok((refreshed.access_token, true));
                }
                Err(e) => {
                    jcode_base::logging::error(&format!("OAuth token refresh failed: {}", e));
                    // A still-unexpired token may remain usable until its
                    // actual expiry even when a proactive refresh fails. Once
                    // expired, however, returning it only guarantees a 401 and
                    // prevents Auto mode from trying its API-key fallback.
                    if fresh_creds.expires_at <= now {
                        anyhow::bail!("Claude OAuth token is expired and refresh failed: {e:#}");
                    }
                }
            }
        }

        if fresh_creds.expires_at <= now {
            anyhow::bail!(
                "Claude OAuth token is expired and no usable refresh token is available. Run `jcode login --provider claude` to refresh OAuth credentials"
            );
        }

        // Cache and return the still-usable loaded credentials.
        let mut cached = self.credentials.write().await;
        *cached = Some(CachedCredentials {
            access_token: fresh_creds.access_token.clone(),
            refresh_token: fresh_creds.refresh_token,
            expires_at: fresh_creds.expires_at,
            account_label: Some(active_label.unwrap_or_else(auth::claude::primary_account_label)),
        });

        Ok((fresh_creds.access_token, true))
    }

    pub(crate) fn set_credential_mode(&self, mode: AnthropicCredentialMode) -> Result<()> {
        match mode {
            AnthropicCredentialMode::Auto => {}
            AnthropicCredentialMode::ApiKey => {
                load_anthropic_api_key()?;
            }
            AnthropicCredentialMode::OAuth => {
                auth::claude::load_credentials().context("Failed to load Claude credentials")?;
            }
        }
        let mut mode_guard = self.credential_mode.try_write().map_err(|_| {
            anyhow::anyhow!(
                "Cannot change Anthropic credential mode while a request is in progress"
            )
        })?;
        *mode_guard = mode;
        drop(mode_guard);
        if let Ok(mut cached) = self.credentials.try_write() {
            *cached = None;
        }
        // Keep the runtime provider identity in sync with the explicit credential
        // choice so UI surfaces (model picker, header widget) report the auth
        // method that requests will actually use, instead of inferring it from
        // credential presence. `Auto` leaves the existing identity untouched.
        if let Some(route) = mode.auth_route(jcode_provider_core::DualAuthProvider::Anthropic) {
            jcode_base::env::set_var("JCODE_RUNTIME_PROVIDER", route.runtime_provider_key());
        }
        // Drop any cached auth snapshot so surfaces that still consult the cheap
        // cached probe (auto-mode resolution, usage availability, account labels)
        // re-derive from the new credential choice on their next read instead of
        // lingering on a snapshot taken before the switch.
        jcode_base::auth::AuthStatus::invalidate_cached_status();
        Ok(())
    }

    pub(crate) fn credential_mode_snapshot(&self) -> AnthropicCredentialMode {
        self.credential_mode
            .try_read()
            .map(|mode| *mode)
            .unwrap_or(AnthropicCredentialMode::Auto)
    }

    /// Convert our Message type to Anthropic API format
    /// Also repairs dangling tool_uses by injecting synthetic tool_results
    fn format_messages(&self, messages: &[Message], is_oauth: bool) -> Vec<ApiMessage> {
        jcode_provider_anthropic::format_messages(messages, is_oauth)
    }

    /// Convert our ContentBlock to Anthropic API format
    #[cfg(test)]
    fn format_content_blocks(
        &self,
        blocks: &[ContentBlock],
        is_oauth: bool,
    ) -> Vec<ApiContentBlock> {
        jcode_provider_anthropic::format_content_blocks(blocks, is_oauth)
    }

    /// Convert tool definitions to Anthropic API format
    /// Adds cache_control to the last tool for prompt caching
    fn format_tools(&self, tools: &[ToolDefinition], is_oauth: bool) -> Vec<ApiTool> {
        jcode_provider_anthropic::format_tools(tools, is_oauth, is_cache_ttl_1h())
    }
}

impl Default for AnthropicProvider {
    fn default() -> Self {
        Self::new()
    }
}

fn log_anthropic_canonical_input(
    model: &str,
    format: &str,
    request: &ApiRequest,
    is_oauth: bool,
    split_prompt: bool,
) {
    let messages_value = serde_json::to_value(&request.messages).unwrap_or(Value::Null);
    let message_items = messages_value.as_array().cloned().unwrap_or_default();
    let system_value = request
        .system
        .as_ref()
        .and_then(|system| serde_json::to_value(system).ok());
    let tools_value = request
        .tools
        .as_ref()
        .and_then(|tools| serde_json::to_value(tools).ok());
    let payload = json!({
        "model": &request.model,
        "max_tokens": request.max_tokens,
        "system": system_value.as_ref(),
        "messages": messages_value,
        "tools": tools_value.as_ref(),
        "thinking": &request.thinking,
        "output_config": &request.output_config,
        "temperature": request.temperature,
        "speed": &request.speed,
    });

    jcode_provider_core::fingerprint::log_provider_canonical_input(
        "anthropic",
        model,
        format,
        &payload,
        &message_items,
        system_value.as_ref(),
        tools_value.as_ref(),
        request.tools.as_ref().map(|tools| tools.len()),
        &[
            ("oauth", is_oauth.to_string()),
            ("split_prompt", split_prompt.to_string()),
        ],
    );
}

#[async_trait]
impl Provider for AnthropicProvider {
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let (token, is_oauth) = self.get_access_token().await?;
        if is_oauth {
            ensure_oauth_preflight(
                &self.client,
                &token,
                &self.oauth_session_id,
                &self.oauth_preflight_done,
            )
            .await?;
        }
        let selected_model = self
            .model
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let model = self
            .model_after_oauth_quota_check(&token, is_oauth, selected_model)
            .await;
        let api_model = strip_1m_suffix(&model).to_string();

        // Format request
        let api_messages = self.format_messages(messages, is_oauth);
        let api_tools = self.format_tools(tools, is_oauth);
        let (thinking, output_config, temperature) =
            self.build_reasoning_request_parts(&model, is_oauth);

        let request = ApiRequest {
            model: api_model,
            max_tokens: self.max_tokens_for(&model),
            system: build_system_param(system, is_oauth),
            messages: format_messages_with_identity(api_messages, is_oauth),
            tools: if api_tools.is_empty() {
                None
            } else {
                Some(api_tools)
            },
            metadata: if is_oauth {
                Some(oauth_request_metadata(&self.oauth_session_id))
            } else {
                None
            },
            thinking,
            output_config,
            temperature,
            // Priority Tier is a separate feature, unsupported on Opus 5
            // and mutually exclusive with fast mode: never send it here.
            service_tier: None,
            speed: self.fast_speed_for_model(&model),
            stream: true,
        };

        log_anthropic_canonical_input(&model, "anthropic_messages", &request, is_oauth, false);

        jcode_base::logging::info(&format!(
            "Anthropic transport: HTTPS SSE stream (oauth={})",
            is_oauth
        ));

        // Create channel for streaming events
        let (tx, rx) = mpsc::channel::<Result<StreamEvent>>(100);

        // Clone what we need for the async task
        let client = self.client.clone();
        let credentials = Arc::clone(&self.credentials);
        let oauth_session_id = self.oauth_session_id.clone();
        let model_state = Arc::clone(&self.model);
        let direct_transport = self.direct_transport.clone();

        // Spawn task to handle streaming with retry logic.
        // This includes forced OAuth refresh on auth failures.
        tokio::spawn(async move {
            if tx
                .send(Ok(StreamEvent::ConnectionType {
                    connection: "https/sse".to_string(),
                }))
                .await
                .is_err()
            {
                return;
            }
            run_stream_with_retries(
                client,
                token,
                is_oauth,
                request,
                tx,
                credentials,
                model,
                oauth_session_id,
                model_state,
                direct_transport,
            )
            .await;
        });

        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    fn model(&self) -> String {
        self.model
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn credential_mode(&self) -> jcode_provider_core::CredentialMode {
        self.credential_mode_snapshot()
    }

    fn set_credential_mode(&self, mode: jcode_provider_core::CredentialMode) -> Result<()> {
        AnthropicProvider::set_credential_mode(self, mode)
    }

    fn set_model(&self, model: &str) -> Result<()> {
        // Native-1M models (Opus 4.8/4.7) no longer carry a redundant `[1m]`
        // alias. Gracefully migrate a stale `<model>[1m]` id (from old config or
        // a restored session) to its canonical form, since the suffix is a no-op
        // for these models.
        let model: &str = if is_1m_model(model)
            && matches!(
                jcode_provider_core::anthropic_context_mode(model),
                jcode_provider_core::AnthropicContextMode::Native1M
            ) {
            strip_1m_suffix(model)
        } else {
            model
        };
        if !self
            .profile_models
            .as_ref()
            .is_some_and(|models| models.iter().any(|configured| configured == model))
            && !jcode_base::provider::known_anthropic_model_ids()
                .iter()
                .any(|known| known == model)
        {
            anyhow::bail!("Model {} not supported by Anthropic provider", model);
        }
        *self
            .model
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = model.to_string();
        match self.reasoning_effort.write() {
            Ok(mut guard) => {
                if let Some(current) = guard.clone() {
                    *guard = Some(Self::store_effort_for_model(model, &current));
                }
            }
            Err(poisoned) => {
                let mut guard = poisoned.into_inner();
                if let Some(current) = guard.clone() {
                    *guard = Some(Self::store_effort_for_model(model, &current));
                }
            }
        }
        Ok(())
    }

    fn available_models(&self) -> Vec<&'static str> {
        AVAILABLE_MODELS.to_vec()
    }

    fn available_models_for_switching(&self) -> Vec<String> {
        jcode_base::provider::cached_anthropic_model_ids()
            .unwrap_or_else(jcode_base::provider::known_anthropic_model_ids)
    }

    fn available_models_display(&self) -> Vec<String> {
        self.available_models_for_switching()
    }

    fn reasoning_effort(&self) -> Option<String> {
        let model = self.model();
        if !Self::model_supports_reasoning_effort(&model) {
            return None;
        }
        // Surface the *effective* effort so the UI/status reflects the Opus
        // default (e.g. `xhigh`) when the user has not picked one explicitly.
        self.effort_for_model(&model)
    }

    fn set_reasoning_effort(&self, effort: &str) -> Result<()> {
        let requested = effort.trim().to_ascii_lowercase();
        if !requested.is_empty()
            && !matches!(
                requested.as_str(),
                "default"
                    | "auto"
                    | "off"
                    | "disabled"
                    | "none"
                    | "low"
                    | "medium"
                    | "high"
                    | "xhigh"
                    | "max"
                    | "swarm"
                    | "swarm-deep"
            )
        {
            anyhow::bail!(
                "Unsupported Anthropic reasoning effort '{}'; expected none|low|medium|high|xhigh|max|swarm|swarm-deep",
                effort
            );
        }
        let normalized = Self::normalize_reasoning_effort(effort);
        let model = self.model();
        if normalized.is_some() && !Self::model_supports_reasoning_effort(&model) {
            anyhow::bail!(
                "Reasoning effort is only supported for Claude 3.7 reasoning models and Claude 4.5+ models that expose Anthropic thinking/output_config"
            );
        }
        if normalized.as_deref() == Some("xhigh") && !Self::model_supports_xhigh_effort(&model) {
            anyhow::bail!(
                "Anthropic xhigh effort is not supported by this model (available on Opus 4.7+, Sonnet 5+, and Fable 5+)"
            );
        }
        let normalized = normalized.map(|effort| Self::store_effort_for_model(&model, &effort));
        match self.reasoning_effort.write() {
            Ok(mut guard) => {
                *guard = normalized;
                Ok(())
            }
            Err(poisoned) => {
                *poisoned.into_inner() = normalized;
                Ok(())
            }
        }
    }

    fn available_efforts(&self) -> Vec<&'static str> {
        let model = self.model();
        if !Self::model_supports_reasoning_effort(&model) {
            return vec![];
        }
        let mut efforts = vec!["none", "low", "medium", "high"];
        if Self::model_supports_xhigh_effort(&model) {
            efforts.push("xhigh");
        }
        if Self::model_supports_max_effort(&model) {
            efforts.push("max");
        }
        efforts.extend(["swarm", "swarm-deep"]);
        efforts
    }

    fn service_tier(&self) -> Option<String> {
        // The trait method keeps the `service_tier` name for UI/protocol
        // compatibility, but Anthropic's feature here is fast mode. `priority`
        // is the shared cross-provider spelling used by `/fast`.
        self.fast_speed_for_model(&self.model())
            .map(|_| "priority".to_string())
    }

    fn set_service_tier(&self, service_tier: &str) -> Result<()> {
        let normalized = Self::normalize_service_tier(service_tier)?;
        if normalized.is_some() && !Self::model_supports_fast_mode(&self.model()) {
            anyhow::bail!(
                "Anthropic fast mode is only supported on Claude Opus 5 and Claude Opus 4.8"
            );
        }
        *self
            .service_tier
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = normalized;
        Ok(())
    }

    fn available_service_tiers(&self) -> Vec<&'static str> {
        if Self::model_supports_fast_mode(&self.model()) {
            vec!["off", "priority"]
        } else {
            vec![]
        }
    }

    async fn prefetch_models(&self) -> Result<()> {
        if self.direct_transport.api_url != API_URL {
            // Named Anthropic-compatible profiles use their configured static
            // model list. Never send gateway credentials to Anthropic's
            // official hard-coded model-catalog endpoint.
            return Ok(());
        }
        let (token, is_oauth) = self.get_access_token().await?;
        if token.trim().is_empty() {
            return Ok(());
        }

        let catalog = if is_oauth {
            match jcode_base::provider::fetch_anthropic_model_catalog_oauth(&token).await {
                Ok(catalog) => Ok(catalog),
                Err(err) if is_oauth_catalog_auth_error(&err.to_string()) => {
                    jcode_base::logging::info(
                        "Anthropic OAuth model catalog auth failed; forcing token refresh and retrying...",
                    );
                    let refreshed_token =
                        force_refresh_oauth_token(Arc::clone(&self.credentials)).await?;
                    jcode_base::provider::fetch_anthropic_model_catalog_oauth(&refreshed_token)
                        .await
                }
                Err(err) => Err(err),
            }
        } else {
            jcode_base::provider::fetch_anthropic_model_catalog(&token).await
        };
        let catalog = match catalog {
            Ok(catalog) => catalog,
            Err(err) => {
                let credential_label = if is_oauth { "OAuth" } else { "API key" };
                jcode_base::logging::warn(&format!(
                    "Anthropic {credential_label} model catalog refresh failed; keeping fallback list: {}",
                    err
                ));
                return Ok(());
            }
        };
        jcode_base::provider::persist_anthropic_model_catalog(&catalog);
        if !catalog.context_limits.is_empty() {
            jcode_base::provider::populate_context_limits(catalog.context_limits);
        }
        if !catalog.available_models.is_empty() {
            jcode_base::provider::populate_anthropic_models(catalog.available_models);
        }
        Ok(())
    }

    fn name(&self) -> &'static str {
        "anthropic"
    }

    fn context_window(&self) -> usize {
        context_window::resolve(&self.model())
    }

    fn supports_image_input(&self) -> bool {
        true
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            client: self.client.clone(),
            model: Arc::new(std::sync::RwLock::new(
                self.model
                    .read()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone(),
            )),
            reasoning_effort: Arc::new(std::sync::RwLock::new(self.stored_reasoning_effort())),
            service_tier: Arc::new(std::sync::RwLock::new(self.service_tier())),
            credentials: Arc::new(RwLock::new(None)),
            credential_mode: Arc::clone(&self.credential_mode),
            max_tokens_override: self.max_tokens_override,
            oauth_session_id: self.oauth_session_id.clone(),
            oauth_preflight_done: Arc::new(AtomicBool::new(
                self.oauth_preflight_done.load(Ordering::Relaxed),
            )),
            direct_transport: self.direct_transport.clone(),
            profile_api_key: self.profile_api_key.clone(),
            profile_models: self.profile_models.clone(),
        })
    }

    async fn invalidate_credentials(&self) {
        let mut cached = self.credentials.write().await;
        *cached = None;
    }

    fn native_result_sender(&self) -> Option<NativeToolResultSender> {
        None // Direct API doesn't use native tool bridge
    }

    /// Split system prompt completion for better cache efficiency
    /// Static content is cached, dynamic content is not
    async fn complete_split(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        system_static: &str,
        system_dynamic: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let (token, is_oauth) = self.get_access_token().await?;
        if is_oauth {
            ensure_oauth_preflight(
                &self.client,
                &token,
                &self.oauth_session_id,
                &self.oauth_preflight_done,
            )
            .await?;
        }
        let selected_model = self
            .model
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let model = self
            .model_after_oauth_quota_check(&token, is_oauth, selected_model)
            .await;
        let api_model = strip_1m_suffix(&model).to_string();

        // Format request
        let api_messages = self.format_messages(messages, is_oauth);
        let api_tools = self.format_tools(tools, is_oauth);
        let (thinking, output_config, temperature) =
            self.build_reasoning_request_parts(&model, is_oauth);

        let request = ApiRequest {
            model: api_model,
            max_tokens: self.max_tokens_for(&model),
            system: build_system_param_split(system_static, system_dynamic, is_oauth),
            messages: format_messages_with_identity(api_messages, is_oauth),
            tools: if api_tools.is_empty() {
                None
            } else {
                Some(api_tools)
            },
            metadata: if is_oauth {
                Some(oauth_request_metadata(&self.oauth_session_id))
            } else {
                None
            },
            thinking,
            output_config,
            temperature,
            // Priority Tier is a separate feature, unsupported on Opus 5
            // and mutually exclusive with fast mode: never send it here.
            service_tier: None,
            speed: self.fast_speed_for_model(&model),
            stream: true,
        };

        log_anthropic_canonical_input(&model, "anthropic_messages_split", &request, is_oauth, true);

        jcode_base::logging::info(&format!(
            "Anthropic transport: HTTPS SSE split stream (oauth={})",
            is_oauth
        ));

        // Create channel for streaming events
        let (tx, rx) = mpsc::channel::<Result<StreamEvent>>(100);

        // Clone what we need for the async task
        let client = self.client.clone();
        let credentials = Arc::clone(&self.credentials);
        let oauth_session_id = self.oauth_session_id.clone();
        let model_state = Arc::clone(&self.model);
        let direct_transport = self.direct_transport.clone();

        // Spawn task to handle streaming with retry logic
        tokio::spawn(async move {
            if tx
                .send(Ok(StreamEvent::ConnectionType {
                    connection: "https/sse".to_string(),
                }))
                .await
                .is_err()
            {
                return;
            }
            run_stream_with_retries(
                client,
                token,
                is_oauth,
                request,
                tx,
                credentials,
                model,
                oauth_session_id,
                model_state,
                direct_transport,
            )
            .await;
        });

        Ok(Box::pin(ReceiverStream::new(rx)))
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "stream retry helper needs auth/session/runtime knobs together and is kept local for clarity"
)]
async fn run_stream_with_retries(
    client: Client,
    initial_token: String,
    is_oauth: bool,
    mut request: ApiRequest,
    tx: mpsc::Sender<Result<StreamEvent>>,
    credentials: Arc<RwLock<Option<CachedCredentials>>>,
    model_name: String,
    oauth_session_id: String,
    model_state: Arc<std::sync::RwLock<String>>,
    direct_transport: DirectTransportConfig,
) {
    let mut token = initial_token;
    let mut last_error = None;
    let mut next_retry_delay = None;
    let mut attempted_forced_refresh = false;
    let original_model = model_name.clone();
    let mut model_name = model_name;
    // Track every model id we have already attempted so a retired/renamed
    // model only falls back to genuinely new candidates.
    let mut tried_models: Vec<String> = vec![original_model.clone()];

    // Account rotation state. The override is put back only when rotation ran
    // out of accounts (see `restore_claude_account`): a rotation that landed on
    // a working account must survive an unrelated later failure, or the next
    // turn goes straight back to the account we already know is limited.
    let account_rotation_enabled =
        is_oauth && jcode_base::provider::prefer_account_failover_enabled();
    let original_account = if account_rotation_enabled {
        auth::claude::active_account_label()
    } else {
        None
    };
    let mut tried_accounts: Vec<String> = if account_rotation_enabled {
        // With no override set, requests run on the primary account, so that is
        // the label rotation must not hand back.
        vec![
            original_account
                .clone()
                .unwrap_or_else(auth::claude::primary_account_label),
        ]
    } else {
        Vec::new()
    };
    // Account the in-flight attempt is running on, so a second rotation reports
    // the account it actually left rather than the one the request started on,
    // and so the restore below can tell our own override apart from one another
    // session set meanwhile.
    let mut current_account = original_account.clone();
    let mut rotation_exhausted = false;

    // Attempts at the same request. Each successful account rotation grants one
    // extra attempt, so rotating never consumes the retries reserved for
    // transient network faults, and the other one-shot retry causes in this
    // loop (forced token refresh, model fallback, reasoning strip) cannot
    // starve rotation of the attempt it needs.
    let mut attempt_budget = MAX_RETRIES;
    // Transport-level retries, counted separately so `MAX_RETRIES` keeps
    // meaning "attempts at the same request", as before.
    let mut transport_retries: u32 = 0;
    // Capacity retries, counted separately again: an overload burst and a flaky
    // network are independent faults and must not consume each other's budget.
    let mut overloaded_retries: u32 = 0;
    // Rate-limit retries, counted separately for the same reason: a burst limit
    // that outlived account rotation is not a transport fault and must not be
    // capped by the transport budget.
    let mut rate_limit_retries: u32 = 0;
    // Whether the "every account is out of quota, earliest reset X" notice has
    // been shown. Once per turn: the branch that emits it is reached by every
    // later failing attempt.
    let mut announced_quota_exhaustion = false;
    // This request's slot in the pool-recovery wake-up spread. Allocated once,
    // so the request keeps its slot across retries instead of colliding with a
    // sibling on a later attempt.
    let pool_recovery_salt = POOL_RECOVERY_SALT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut attempt: u32 = 0;

    while attempt < attempt_budget {
        // Consumed up front: the branches below reach `continue` from several
        // places, and every one of them must count as an attempt spent.
        let attempt_index = attempt;
        attempt += 1;

        if attempt_index > 0 {
            // Exponential backoff with jitter: ~1s, ~2s, ~4s
            let delay = jcode_provider_core::retry_after::retry_delay(
                attempt_index,
                RETRY_BASE_DELAY_MS,
                next_retry_delay.take(),
            );
            let _ = tx
                .send(Ok(StreamEvent::ConnectionPhase {
                    phase: jcode_message_types::ConnectionPhase::Retrying {
                        attempt: attempt_index + 1,
                        max: attempt_budget,
                    },
                }))
                .await;
            tokio::time::sleep(clamp_retry_delay(delay)).await;
            jcode_base::logging::info(&format!(
                "Retrying Anthropic API request (attempt {}/{})",
                attempt_index + 1,
                attempt_budget
            ));
        }

        // Track whether this attempt streams replay-visible output so a
        // mid-stream transport fault can roll the partial output back on the
        // consumer before the retry replays the response from the top.
        let (attempt_tx, attempt_guard) =
            jcode_provider_core::attempt_tracker::track_attempt_output(tx.clone());

        // Retries use a fresh unpooled client: the fault that broke attempt N
        // (e.g. TLS BadRecordMac from a corrupting middlebox) may also have
        // poisoned other idle pooled connections opened through the same path,
        // so reusing the shared pool can fail identically. A fresh client
        // guarantees a brand-new TCP+TLS connection.
        let attempt_client = if attempt_index == 0 {
            client.clone()
        } else {
            jcode_provider_core::fresh_transport_client()
        };

        // When this attempt set out. A success proves the account was serving at
        // this instant, not now: a streaming turn can run for minutes, and the
        // pool can hit its limit meanwhile. Used to refuse clearing a cooldown
        // that was recorded *after* this point. See ADR-0023.
        let attempt_started = std::time::Instant::now();

        match stream_response(
            attempt_client,
            token.clone(),
            is_oauth,
            request.clone(),
            attempt_tx,
            &model_name,
            &oauth_session_id,
            &direct_transport,
        )
        .await
        {
            Ok(()) => {
                let _ = attempt_guard.finish().await;
                // This account served *this* request, so drop any cooldown and
                // cached limit verdict that predate it, rather than avoiding the
                // account for the full window.
                //
                // Conditional on the cooldown being older than this attempt. A
                // long stream that started before the pool tipped over would
                // otherwise erase the refusals its siblings collected while it
                // ran, and the next request would rotate straight back onto the
                // limited account: the observed
                // `claude-1 → claude-2 → claude-1` ladder. See ADR-0023.
                if account_rotation_enabled
                    && let Some(label) = current_account
                        .clone()
                        .or_else(|| Some(auth::claude::primary_account_label()))
                {
                    let cleared = jcode_base::provider::clear_account_rate_limit_if_newer(
                        jcode_provider_core::ActiveProvider::Claude,
                        &label,
                        attempt_started,
                    );
                    if !cleared {
                        jcode_base::logging::info(&format!(
                            "Keeping the Anthropic cooldown on '{label}': it was recorded after this request started, so this success is older evidence"
                        ));
                    }
                }
                return; // Success
            }
            Err(e) => {
                let saw_output = attempt_guard.finish().await;
                // Use the full anyhow source chain ({:#}) rather than just the top
                // context. The underlying cause (e.g. the HTTP/2 "stream error" or
                // a connection reset) lives deeper than "Failed to send request to
                // Anthropic API", and the retry classifier needs to see it.
                let error_str = format!("{e:#}").to_lowercase();

                // OAuth auth failures: force refresh and retry once immediately.
                if is_oauth && is_oauth_auth_error(&error_str) && !attempted_forced_refresh {
                    attempted_forced_refresh = true;
                    jcode_base::logging::info(
                        "Anthropic OAuth authentication failed, forcing token refresh...",
                    );
                    let _ = tx
                        .send(Ok(StreamEvent::ConnectionPhase {
                            phase: jcode_message_types::ConnectionPhase::Authenticating,
                        }))
                        .await;
                    match force_refresh_oauth_token(Arc::clone(&credentials)).await {
                        Ok(refreshed_token) => {
                            jcode_base::logging::info(
                                "Forced OAuth token refresh succeeded, retrying request.",
                            );
                            token = refreshed_token;
                            last_error = Some(e);
                            continue;
                        }
                        Err(refresh_err) => {
                            restore_claude_account(
                                &original_account,
                                &current_account,
                                rotation_exhausted,
                            );
                            let _ = tx
                                .send(Err(anyhow::anyhow!(
                                    "{}\n\nAutomatic Claude OAuth refresh failed: {}\nRun `jcode login --provider claude` (preferred) or `claude`, then retry.",
                                    e,
                                    refresh_err
                                )))
                                .await;
                            return;
                        }
                    }
                }

                // Model not found (e.g. a retired or renamed model id): the
                // server rejects the request up front with a 404 before any
                // output streams. Transparently fall back to the *best* available
                // model so the in-flight request still completes instead of
                // hard-failing, and persist the switch so later turns reuse the
                // working model. The fallback honors any server "Please use X"
                // recommendation, then the curated flagship-first quality order,
                // and never downgrades to a cheaper tier when a stronger model is
                // available (see `anthropic_fallback_model`).
                if is_model_not_found_error(&error_str)
                    && !saw_output
                    && let Some(fallback) = anthropic_fallback_model(&tried_models, &error_str)
                {
                    jcode_base::logging::warn(&format!(
                        "Anthropic model '{}' is not available ({}); retrying with fallback '{}'",
                        model_name, e, fallback
                    ));
                    // Surface the substitution so the user is not silently moved
                    // to a different model than they selected.
                    let _ = tx
                        .send(Ok(StreamEvent::StatusDetail {
                            detail: format!(
                                "⚠ '{}' is unavailable; falling back to '{}'",
                                strip_1m_suffix(&model_name),
                                strip_1m_suffix(&fallback)
                            ),
                        }))
                        .await;
                    request.model = strip_1m_suffix(&fallback).to_string();
                    *model_state
                        .write()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()) = fallback.clone();
                    tried_models.push(fallback.clone());
                    model_name = fallback;
                    last_error = Some(e);
                    continue;
                }

                // Fast mode refused (organization not enabled, no usage
                // credits, or access not provisioned in the research preview).
                // Self-heal once by dropping `speed` and retrying at standard
                // speed, exactly as Anthropic's own clients are documented to
                // do, so a fast-mode entitlement problem degrades to a slower
                // answer instead of failing the turn outright.
                if request.speed.is_some()
                    && !saw_output
                    && is_fast_mode_unavailable_error(&error_str)
                {
                    jcode_base::logging::warn(&format!(
                        "Anthropic refused fast mode ({}); retrying at standard speed",
                        e
                    ));
                    request.speed = None;
                    last_error = Some(e);
                    continue;
                }

                // Anthropic OAuth can reject Fable with a model-scoped weekly
                // quota error before the usage cache observes the exhausted
                // window. This is terminal for Fable, not a transient 429.
                if is_oauth
                    && !saw_output
                    && is_fable_scoped_limit_error(&model_name, &error_str)
                    && let Some(fallback) =
                        AnthropicProvider::best_available_opus_model(&model_name)
                {
                    jcode_base::logging::warn(&format!(
                        "Anthropic Fable weekly quota is exhausted ({}); retrying with '{}'",
                        e, fallback
                    ));
                    let _ = tx
                        .send(Ok(StreamEvent::StatusDetail {
                            detail: format!(
                                "⚠ '{}' weekly limit reached; switching to '{}'",
                                strip_1m_suffix(&model_name),
                                strip_1m_suffix(&fallback)
                            ),
                        }))
                        .await;
                    request.model = strip_1m_suffix(&fallback).to_string();
                    *model_state
                        .write()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()) = fallback.clone();
                    tried_models.push(fallback.clone());
                    model_name = fallback;
                    last_error = Some(e);
                    continue;
                }

                // Reasoning request rejected (e.g. a model listed with effort or
                // thinking capabilities that the live API does not actually
                // accept: "adaptive thinking is not supported on this model" or
                // "This model does not support the effort parameter."). Self-heal
                // once by stripping the reasoning fields (and restoring an OAuth
                // temperature, which we omit only because thinking was active)
                // and retrying, so a stale capability table degrades gracefully
                // instead of hard-failing.
                if (request.thinking.is_some() || request.output_config.is_some())
                    && !saw_output
                    && is_reasoning_unsupported_error(&error_str)
                {
                    jcode_base::logging::warn(&format!(
                        "Anthropic model '{}' rejected the reasoning request ({}); retrying without thinking/effort",
                        model_name, e
                    ));
                    request.thinking = None;
                    request.output_config = None;
                    if is_oauth {
                        request.temperature = Some(1.0);
                    }
                    last_error = Some(e);
                    continue;
                }

                // Rate limit / usage exhaustion on an OAuth account. This error
                // arrives as an item inside the event stream, long after
                // `complete()` returned the stream, so
                // `MultiProvider::try_same_provider_account_failover` never
                // sees it and cannot rotate accounts for us. Without the
                // rotation below the only remaining reaction is the TUI
                // fallback picker offering a *smaller model* on the same
                // exhausted account. Replay the same model on the next account
                // instead; only when every account is limited does the caller
                // fall through to a model downgrade.
                if should_rotate_account(account_rotation_enabled, &error_str) {
                    // Remember which account refused, so the *next* request does
                    // not rotate straight back into it. Without this, each
                    // request starts with an empty `tried_accounts` and
                    // re-discovers the same limit, burning a request per hop.
                    //
                    // This records the limit in the per-account usage cache too,
                    // which is what the rotation decision reads. Recording only
                    // the cooldown left a stale "has headroom" snapshot in place,
                    // so the candidate list kept offering the blocked account
                    // and the "every account is spent" fast-fail never fired.
                    // See ADR-0017.
                    if let Some(label) = current_account
                        .clone()
                        .or_else(|| Some(auth::claude::primary_account_label()))
                    {
                        jcode_base::provider::note_account_rate_limited_with_reset(
                            jcode_provider_core::ActiveProvider::Claude,
                            &label,
                            account_reset_hint(&label),
                        );
                    }
                    match switch_to_next_claude_account(&credentials, &mut tried_accounts).await {
                        Some(switched) => {
                            let AccountSwitch {
                                label: switched_label,
                                token: switched_token,
                                claimed_shared_override,
                            } = switched;
                            let previous_label = current_account
                                .clone()
                                .unwrap_or_else(auth::claude::primary_account_label);
                            current_account = Some(switched_label.clone());
                            // The client renders usage from its own process and
                            // cannot see this process's account override, so a
                            // rotation is invisible to it unless announced.
                            //
                            // Announced only when this request actually moved the
                            // shared override. Otherwise the switch is local to
                            // this request, and the broadcast carries no session
                            // id (ADR-0011), so it would repoint every other
                            // session's usage widget at an account they are not
                            // on. See ADR-0018.
                            if claimed_shared_override {
                                jcode_base::bus::Bus::global().publish_anthropic_account_rotated(
                                    switched_label.clone(),
                                    Some(previous_label.clone()),
                                );
                            }
                            jcode_base::logging::info(&format!(
                                "Same-provider account failover: Anthropic rate-limited ({}); retrying '{}' on account '{}'",
                                e, model_name, switched_label
                            ));
                            if saw_output {
                                // The limit hit after partial output reached the
                                // consumer. Same contract as a mid-stream
                                // transport fault: tell it to discard the
                                // partial attempt so the replay on the new
                                // account cannot duplicate what it already saw.
                                let _ = tx
                                    .send(Ok(StreamEvent::RetryRollback {
                                        attempt: attempt_index + 2,
                                        max: attempt_budget + 1,
                                    }))
                                    .await;
                            }
                            let _ = tx
                                .send(Ok(StreamEvent::StatusDetail {
                                    detail: format!(
                                        "⚡ Rate limited; switching Claude account: {} → {}",
                                        previous_label, switched_label
                                    ),
                                }))
                                .await;
                            token = switched_token;
                            // This rotation earns its own attempt rather than
                            // spending one the transient-retry path may need.
                            attempt_budget += 1;
                            // The new account has its own quota bucket, so the
                            // `retry-after` the exhausted account asked for
                            // (often 60s) does not apply to it.
                            next_retry_delay = None;
                            last_error = Some(e);
                            continue;
                        }
                        None => {
                            // Every account is limited. Remember it, so the
                            // failure paths below hand the override back instead
                            // of leaving the session on an account it only
                            // reached while searching for a working one.
                            rotation_exhausted = true;
                        }
                    }
                }

                // Every account answered 429 (or rotation is off). Anthropic's
                // short-window burst limit clears on the order of seconds, and
                // the account quotas may be nowhere near spent, so wait it out
                // on a rate-limit-shaped clock instead of dropping through to
                // the ~200ms transport backoff and killing the turn.
                if jcode_base::provider::error_looks_like_usage_limit(&error_str)
                    && rate_limit_retries + 1 < MAX_RATE_LIMIT_RETRIES
                    // A spent quota does not clear for hours, so waiting is
                    // pure delay: fail now and let the caller downgrade or
                    // switch provider while the user watches. Only an
                    // unexplained 429 (burst limit) is worth outlasting.
                    && !jcode_base::provider::all_accounts_quota_exhausted(
                        jcode_provider_core::ActiveProvider::Claude,
                    )
                {
                    rate_limit_retries += 1;
                    if saw_output {
                        let _ = tx
                            .send(Ok(StreamEvent::RetryRollback {
                                attempt: attempt_index + 2,
                                max: attempt_budget + 1,
                            }))
                            .await;
                    }
                    // A server `retry-after` still wins, but capped: Anthropic
                    // sends the same `retry-after: 60` for a burst as for a
                    // spent quota, and four consecutive minutes of silence is
                    // the wrong response to either. See ADR-0016.
                    //
                    // The pool's own cooldown wins over both when it is known.
                    // Every account is refusing and this process already knows
                    // when the first of them comes back, so retrying before then
                    // is a guaranteed 429 that also re-extends the cooldown it
                    // re-proves. With several sessions open that noise is
                    // multiplied by the session count, which is what the user
                    // sees as a wall of switch lines. Waiting for the real
                    // deadline collapses it to one request at the moment it can
                    // succeed. Clamped so a long quota reset does not turn into
                    // an unbounded silent sleep: past the cap the loop gives up
                    // and the caller can downgrade or switch provider.
                    // See ADR-0023.
                    let pool_delay = jcode_base::provider::time_until_any_account_is_eligible(
                        jcode_provider_core::ActiveProvider::Claude,
                    )
                    .filter(|wait| *wait <= MAX_POOL_RECOVERY_WAIT)
                    // Spread the wake-up: every session computed this same
                    // deadline from the same shared cooldown.
                    .map(|wait| wait + pool_recovery_spread(pool_recovery_salt));
                    let delay = pool_delay.unwrap_or_else(|| {
                        jcode_provider_core::retry_after::retry_after_from_error(&e)
                            .map(|hint| hint.min(MAX_BURST_RETRY_AFTER))
                            .unwrap_or_else(|| {
                                jcode_provider_core::attempt_tracker::retry_backoff_delay(
                                    rate_limit_retries,
                                    RATE_LIMIT_RETRY_BASE_DELAY_MS,
                                )
                            })
                    });
                    jcode_base::logging::info(&format!(
                        "Anthropic rate limited on every account (attempt {}/{}), waiting {:?}: {}",
                        rate_limit_retries, MAX_RATE_LIMIT_RETRIES, delay, e
                    ));
                    let _ = tx
                        .send(Ok(StreamEvent::StatusDetail {
                            detail: format!(
                                "⏳ Every account rate limited; waiting {}s and retrying ({}/{})",
                                delay.as_secs().max(1),
                                rate_limit_retries,
                                MAX_RATE_LIMIT_RETRIES
                            ),
                        }))
                        .await;
                    // Waiting out a shared limit is not the user's turn failing,
                    // so it must not consume a transport attempt.
                    attempt_budget += 1;
                    next_retry_delay = Some(delay);
                    last_error = Some(e);
                    continue;
                }

                // Every account's quota really is spent. Waiting cannot help, so
                // say when it *can*: a bare 429 plus an offer to downgrade the
                // model reads as a jcode fault and hides the one fact the user
                // needs, which is that the quota returns at a specific time.
                // Costs nothing on the happy path, and reads only cached usage.
                //
                // Announced at most once per turn: this branch is reached by
                // every subsequent failing attempt, and repeating the same
                // countdown reads as progress when nothing is progressing.
                if !announced_quota_exhaustion
                    && jcode_base::provider::error_looks_like_usage_limit(&error_str)
                    && let Some((label, resets_in)) = jcode_base::provider::earliest_account_reset(
                        jcode_provider_core::ActiveProvider::Claude,
                    )
                {
                    announced_quota_exhaustion = true;
                    let _ = tx
                        .send(Ok(StreamEvent::StatusDetail {
                            detail: format!(
                                "⛔ Every Anthropic account is out of quota; earliest reset {} ({})",
                                resets_in, label
                            ),
                        }))
                        .await;
                }

                // Capacity rejection (`overloaded_error` / HTTP 529). Retryable,
                // but on a longer clock than a transport fault and on its own
                // budget, so a burst of overloads cannot exhaust the retries
                // reserved for network faults (and vice versa).
                if is_overloaded_error(&error_str)
                    && overloaded_retries + 1 < MAX_OVERLOADED_RETRIES
                {
                    overloaded_retries += 1;
                    if saw_output {
                        let _ = tx
                            .send(Ok(StreamEvent::RetryRollback {
                                attempt: attempt_index + 2,
                                max: attempt_budget + 1,
                            }))
                            .await;
                    }
                    jcode_base::logging::info(&format!(
                        "Anthropic is overloaded (attempt {}/{}), backing off and retrying: {}",
                        overloaded_retries, MAX_OVERLOADED_RETRIES, e
                    ));
                    let _ = tx
                        .send(Ok(StreamEvent::StatusDetail {
                            detail: format!(
                                "⏳ Anthropic is overloaded; retrying ({}/{})",
                                overloaded_retries, MAX_OVERLOADED_RETRIES
                            ),
                        }))
                        .await;
                    // An overload is not the user's turn failing, so it must not
                    // consume an attempt the transport retries may still need.
                    attempt_budget += 1;
                    next_retry_delay = Some(
                        jcode_provider_core::retry_after::retry_after_from_error(&e)
                            .unwrap_or_else(|| {
                                jcode_provider_core::attempt_tracker::retry_backoff_delay(
                                    overloaded_retries,
                                    OVERLOADED_RETRY_BASE_DELAY_MS,
                                )
                            }),
                    );
                    last_error = Some(e);
                    continue;
                }

                // Check if this is a transient/retryable error
                if is_retryable_error(&error_str) && transport_retries + 1 < MAX_RETRIES {
                    transport_retries += 1;
                    if saw_output {
                        // The fault hit mid-stream after partial output reached
                        // the consumer. Tell it to discard the partial attempt
                        // so the retried response replays cleanly instead of
                        // duplicating.
                        jcode_base::logging::warn(&format!(
                            "Transient error after partial output; rolling back partial attempt and retrying: {}",
                            e
                        ));
                        let _ = tx
                            .send(Ok(StreamEvent::RetryRollback {
                                attempt: attempt_index + 2,
                                max: attempt_budget,
                            }))
                            .await;
                    } else {
                        jcode_base::logging::info(&format!("Transient error, will retry: {}", e));
                    }
                    next_retry_delay = jcode_provider_core::retry_after::retry_after_from_error(&e);
                    last_error = Some(e);
                    continue;
                }

                // Non-retryable or final attempt
                restore_claude_account(&original_account, &current_account, rotation_exhausted);
                if is_oauth && is_oauth_auth_error(&error_str) {
                    let _ = tx
                        .send(Err(anyhow::anyhow!(
                            "{}\n\nClaude OAuth authentication failed. Run `jcode login --provider claude` (preferred) or `claude`, then retry.",
                            e
                        )))
                        .await;
                } else {
                    let _ = tx.send(Err(e)).await;
                }
                return;
            }
        }
    }

    // All retries exhausted
    restore_claude_account(&original_account, &current_account, rotation_exhausted);
    if let Some(e) = last_error {
        let _ = tx
            .send(Err(anyhow::anyhow!(
                "Failed after {} retries: {}",
                attempt_budget,
                e
            )))
            .await;
    }
}

/// Whether a failed attempt should be replayed on another OAuth account.
///
/// Requires a usage/rate limit: a transport fault is the retry loop's job, not
/// another account's. Partial output does not disqualify a rotation, because the
/// caller emits [`StreamEvent::RetryRollback`] first, exactly as the
/// transient-retry path does for a mid-stream fault.
fn should_rotate_account(rotation_enabled: bool, error_str: &str) -> bool {
    rotation_enabled && jcode_base::provider::error_looks_like_usage_limit(error_str)
}

/// First candidate not yet attempted during this request.
///
/// Candidates arrive best-first (least-used accounts before exhausted ones) and
/// may repeat across calls, because the usage probe is re-read each time; the
/// `tried` list is what guarantees forward progress.
fn next_untried_account<'a>(candidates: &'a [String], tried: &[String]) -> Option<&'a str> {
    candidates
        .iter()
        .map(String::as_str)
        .find(|candidate| !tried.iter().any(|seen| seen == candidate))
}

/// Whether a rotation may rewrite the *shared*, process-wide account override.
///
/// The rotation always moves the request that is rotating; this decides only
/// whether every other session on the daemon is moved with it.
///
/// The rule: claim the shared pointer when the currently-active account is one
/// this request has already proven refuses it, i.e. when this request knows
/// something the pointer does not. If another session has meanwhile settled on
/// an account this request has not tried, leave the pointer alone rather than
/// dragging that session off an account that may be working for it.
///
/// `None` (no override set) means the primary account with nothing yet learned
/// about it, so claiming is correct and matches the previous behaviour.
///
/// Pure and separated from the rotation loop precisely so this policy is
/// testable without two live daemons: the concurrency it guards against reaches
/// this code only as *state another session left behind*, which is exactly the
/// two arguments. See ADR-0018.
fn should_claim_shared_override(active: Option<&str>, tried: &[String]) -> bool {
    match active {
        Some(active) => tried.iter().any(|seen| seen == active),
        None => true,
    }
}

/// Outcome of a successful account rotation.
struct AccountSwitch {
    label: String,
    token: String,
    /// Whether this rotation moved the process-wide override, and so whether the
    /// switch is visible to other sessions on this daemon.
    claimed_shared_override: bool,
}

/// Serialises account rotation across every session on this daemon.
///
/// A `tokio` mutex rather than a `std` one because it is held across the
/// credential load, which awaits; blocking a runtime worker there would stall
/// unrelated sessions. Contention is inherently tiny: rotation happens only on a
/// 429, and the critical section is one credential read.
static ACCOUNT_ROTATION_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Point this request at the next untried account and return its label plus a
/// usable access token.
///
/// Accounts whose credentials cannot be loaded or refreshed are recorded as
/// tried, skipped, and rolled back, so one broken login neither blocks the rest
/// nor leaves the override pointing at an account that cannot serve a request.
/// Returns `None` once no account is left, with the override unchanged from the
/// caller's perspective.
///
/// Rotation is serialised process-wide. One daemon serves every session
/// (ADR-0009), so when a shared limit trips, all of them reach this function
/// within a second or two. Deciding concurrently is a check-then-act race: the
/// cooldown that would warn the others off an account is only recorded once the
/// first 429 comes *back*, and by then everyone has already committed. The
/// observed result (2026-08-14) was four sessions alternating between two
/// accounts inside 25 seconds, each announcing its own switch.
///
/// Holding the lock across the decision makes the winner's move visible to the
/// followers, who then adopt it instead of rotating again. See ADR-0020.
async fn switch_to_next_claude_account(
    credentials: &Arc<RwLock<Option<CachedCredentials>>>,
    tried: &mut Vec<String>,
) -> Option<AccountSwitch> {
    // Held across credential loading (an await) so a follower cannot slip in
    // between the leader claiming the override and that claim being usable.
    let _rotation = ACCOUNT_ROTATION_LOCK.lock().await;

    // Re-read the shared state now that we hold the lock. A sibling session may
    // have rotated the whole pool while this request was queued here, in which
    // case the work is already done and rotating again would just spend another
    // account. Adopting the sibling's account is both correct and free.
    if let Some(active) = auth::claude::active_account_label()
        && !tried.iter().any(|seen| seen == &active)
        && !jcode_base::provider::account_is_cooling_down(
            jcode_provider_core::ActiveProvider::Claude,
            &active,
        )
        && let Ok(fresh) = credentials_for_claude_account(&active).await
    {
        tried.push(active.clone());
        let access_token = fresh.access_token.clone();
        {
            let mut cached = credentials.write().await;
            *cached = Some(fresh);
        }
        jcode_base::logging::info(&format!(
            "Adopting the account a concurrent session already rotated to: '{active}'"
        ));
        return Some(AccountSwitch {
            label: active,
            token: access_token,
            // The leader already announced this move; a second announcement
            // would show the user one switch line per session for a single
            // pool-wide rotation.
            claimed_shared_override: false,
        });
    }

    loop {
        let candidates = jcode_base::provider::account_failover_candidates(
            jcode_provider_core::ActiveProvider::Claude,
        );
        let label = next_untried_account(&candidates, tried)?.to_string();
        tried.push(label.clone());

        let override_before = auth::claude::active_account_label();

        // The override is process-wide and one daemon serves many sessions
        // (ADR-0009), so several sessions sharing two accounts used to push each
        // other back and forth: A rotates everyone to claude-1, B hits a burst
        // there and rotates everyone back to claude-2, and so on, which is the
        // `claude-2 -> claude-1 -> claude-2` flapping users see.
        //
        // A rotation still moves *this* request, but it only rewrites the shared
        // override when it has something better to say. See ADR-0018 and
        // `should_claim_shared_override`.
        let claim_shared_override = should_claim_shared_override(override_before.as_deref(), tried);

        if claim_shared_override {
            auth::claude::set_active_account_override(Some(label.clone()));
            jcode_base::auth::AuthStatus::invalidate_cached_status();
        }

        match credentials_for_claude_account(&label).await {
            Ok(fresh) => {
                let access_token = fresh.access_token.clone();
                let mut cached = credentials.write().await;
                *cached = Some(fresh);
                // Confirm the new account's real headroom in the background, so
                // the *next* decision is based on fresh data rather than on
                // whatever this rotation happened to believe. Deliberately not
                // awaited: a blocking probe here would add a network round trip
                // in front of every rotation, on the exact path the user already
                // experiences as a stall, and the probe endpoint can itself be
                // rate-limited. See ADR-0017.
                spawn_account_usage_refresh(label.clone());
                return Some(AccountSwitch {
                    label,
                    token: access_token,
                    claimed_shared_override: claim_shared_override,
                });
            }
            Err(err) => {
                jcode_base::logging::warn(&format!(
                    "Cannot use Anthropic account '{}' for account failover: {:#}",
                    label, err
                ));
                if claim_shared_override {
                    auth::claude::set_active_account_override(override_before);
                    jcode_base::auth::AuthStatus::invalidate_cached_status();
                }
            }
        }
    }
}

/// Load credentials for `label`, refreshing the access token when it is expired
/// or within the same 5-minute buffer the normal token path uses.
async fn credentials_for_claude_account(label: &str) -> Result<CachedCredentials> {
    let creds = auth::claude::load_credentials_for_account(label)
        .with_context(|| format!("Failed to load Claude credentials for account '{label}'"))?;

    let now = chrono::Utc::now().timestamp_millis();
    if creds.expires_at > now + 300_000 {
        return Ok(CachedCredentials {
            access_token: creds.access_token,
            refresh_token: creds.refresh_token,
            expires_at: creds.expires_at,
            account_label: Some(label.to_string()),
        });
    }

    if creds.refresh_token.is_empty() {
        anyhow::bail!(
            "Claude OAuth token for account '{label}' is expired and has no refresh token"
        );
    }
    let refreshed = oauth::refresh_claude_tokens_for_account(&creds.refresh_token, label)
        .await
        .with_context(|| format!("OAuth refresh failed for Claude account '{label}'"))?;
    Ok(CachedCredentials {
        access_token: refreshed.access_token,
        refresh_token: refreshed.refresh_token,
        expires_at: refreshed.expires_at,
        account_label: Some(label.to_string()),
    })
}

/// Whether a rotation should kick a background usage refresh.
///
/// Always on in production. Off by default under `cfg(test)` and switchable per
/// test, because the refresh is a detached task that writes to a process-global
/// cache: it outlived its own test, deleted the usage snapshots a *later* test
/// had seeded, and replaced them with probe errors, flipping that test's
/// quota-versus-burst verdict. Seen as `a_burst_limit_that_clears_lets_the_turn_finish`
/// failing roughly one run in four while passing under `--test-threads=1`.
#[cfg(test)]
static POST_ROTATION_USAGE_REFRESH: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Enable or disable the post-rotation usage refresh for one test.
#[cfg(test)]
pub(crate) fn set_post_rotation_usage_refresh_for_tests(enabled: bool) {
    POST_ROTATION_USAGE_REFRESH.store(enabled, std::sync::atomic::Ordering::SeqCst);
}

fn post_rotation_usage_refresh_enabled() -> bool {
    #[cfg(test)]
    {
        POST_ROTATION_USAGE_REFRESH.load(std::sync::atomic::Ordering::SeqCst)
    }
    #[cfg(not(test))]
    {
        true
    }
}

/// Refresh `label`'s usage snapshot in the background.
///
/// Fire and forget on purpose: the value is for the *next* decision, not this
/// one, so nothing waits on it and a failure only leaves the verdict unknown,
/// which the rotation logic already handles as "not exhausted".
fn spawn_account_usage_refresh(label: String) {
    if !post_rotation_usage_refresh_enabled() {
        return;
    }
    if tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    tokio::spawn(async move {
        if let Err(err) = jcode_base::usage::refresh_account_usage(&label).await {
            jcode_base::logging::info(&format!(
                "Could not refresh usage for Anthropic account '{}' after rotation: {:#}",
                label, err
            ));
        }
    });
}

/// The reset time already known for `label`, from cached usage only.
///
/// Never fetches. This runs on the failure path of a live request, where a
/// network round trip is exactly the latency the user is complaining about, and
/// the probe endpoint can itself be rate-limited. A missing hint simply means the
/// 429 is treated as an unexplained burst, which is the safe reading.
fn account_reset_hint(label: &str) -> Option<String> {
    jcode_base::usage::cached_anthropic_account_usage_any_age(label)
        .and_then(|usage| usage.blocking_reset_at())
}

/// Whether a finished request still owes the account override a rollback.
///
/// Only a rotation that ran out of accounts does: if the request is sitting on
/// an account that worked, that account is the useful one to keep, and if it
/// never moved there is nothing to undo.
///
/// One exception, and it is the whole point of the dead-end restore being
/// conditional: never hand the session back to an account whose *quota* is
/// spent. ADR-0003 restores on a dead end because when every account is limited
/// the original is as good as any other, which is true for a burst limit that
/// clears in seconds and false for a 5-hour quota with hours left on it. With
/// the reset times now known (ADR-0016) the two cases are distinguishable, so a
/// dead end that ends on a merely burst-limited account keeps it rather than
/// rolling back onto a wall. Without this the *next* turn starts on the blocked
/// account again, which is the cross-turn half of the flapping users report.
fn should_restore_account(
    original: &Option<String>,
    ours: &Option<String>,
    rotation_exhausted: bool,
) -> bool {
    if !rotation_exhausted || ours == original {
        return false;
    }

    // Never hand the session back to an account that just refused it.
    //
    // ADR-0003 restores on a dead end because "when every account is limited the
    // original is as good as any other". That is true only of the *instant* the
    // request ends. It ignores that the next turn then starts on an account
    // already known to be limited, rotates away again, is restored again, and so
    // on: one switch plus one restore per turn, for as long as the limit lasts.
    //
    // Gating on quota alone (the first version of this fix) was too narrow: on a
    // burst limit no account is quota-spent, so the gate never fired and the
    // loop continued. The cooldown is the signal that covers both cases, because
    // every 429 records one, sized to the real reset when known and to a short
    // burst window otherwise.
    //
    // When the original is *not* limited, the restore still happens, which keeps
    // ADR-0003's actual purpose: undoing an exploratory hop that led nowhere.
    let original_label = original
        .clone()
        .unwrap_or_else(auth::claude::primary_account_label);
    !jcode_base::provider::account_is_cooling_down(
        jcode_provider_core::ActiveProvider::Claude,
        &original_label,
    )
}

/// Whether `label`'s cached usage says a window is spent and has not yet reset.
///
/// Reads cache only, at any age; this runs on a request's cleanup path. An
/// unknown verdict answers `false`, so nothing here can strand a session on an
/// account merely because its usage could not be probed.
fn account_quota_is_blocked(label: Option<&str>) -> bool {
    let Some(label) = label else {
        // No original override means the primary account, whose label is what
        // the rest of the rotation machinery uses for exactly this lookup.
        return account_quota_is_blocked(Some(&auth::claude::primary_account_label()));
    };

    let Some(usage) = jcode_base::usage::cached_anthropic_account_usage_any_age(label) else {
        return false;
    };
    if !usage.exhausted() {
        return false;
    }
    usage
        .blocking_reset_at()
        .and_then(|reset| jcode_base::usage::parse_reset_at(&reset))
        .map(|reset| reset > chrono::Utc::now())
        .unwrap_or(false)
}

/// Put the account override back where the request found it.
///
/// Only for the dead end where rotation ran out of accounts: a rotation that
/// produced a working account must stick for the rest of the session (exactly
/// like the account failover in `MultiProvider`), otherwise the next turn goes
/// straight back to the account already known to be limited.
///
/// The override is process-wide and one daemon serves many sessions, so this
/// writes only when the override still holds the label this request put there.
/// Anything else means another session changed accounts meanwhile - including a
/// deliberate `/account switch` - and clobbering that would be worse than
/// leaving our exhausted-account search visible.
fn restore_claude_account(
    original: &Option<String>,
    ours: &Option<String>,
    rotation_exhausted: bool,
) {
    if !should_restore_account(original, ours, rotation_exhausted) {
        return;
    }
    if auth::claude::active_account_label().as_deref() != ours.as_deref() {
        jcode_base::logging::info(
            "Skipping Anthropic account restore: the active account changed while this request was rotating",
        );
        return;
    }
    auth::claude::set_active_account_override(original.clone());
    jcode_base::auth::AuthStatus::invalidate_cached_status();
    jcode_base::bus::Bus::global().publish_anthropic_account_restored(
        original
            .clone()
            .unwrap_or_else(auth::claude::primary_account_label),
        ours.clone(),
    );
}

async fn force_refresh_oauth_token(
    credentials: Arc<RwLock<Option<CachedCredentials>>>,
) -> Result<String> {
    let refresh_from_cache = {
        let cached = credentials.read().await;
        cached
            .as_ref()
            .map(|c| c.refresh_token.clone())
            .filter(|t| !t.is_empty())
    };

    let refresh_token = if let Some(token) = refresh_from_cache {
        token
    } else {
        let loaded = auth::claude::load_credentials()
            .context("Failed to load Claude credentials for forced refresh")?;
        if loaded.refresh_token.is_empty() {
            anyhow::bail!("No refresh token available in Claude credentials");
        }
        loaded.refresh_token
    };

    let active_label =
        auth::claude::active_account_label().unwrap_or_else(auth::claude::primary_account_label);
    let refreshed =
        match oauth::refresh_claude_tokens_for_account(&refresh_token, &active_label).await {
            Ok(refreshed) => refreshed,
            Err(err) => {
                anyhow::bail!("OAuth refresh endpoint rejected the refresh token: {err:#}");
            }
        };

    {
        let mut cached = credentials.write().await;
        *cached = Some(CachedCredentials {
            access_token: refreshed.access_token.clone(),
            refresh_token: refreshed.refresh_token,
            expires_at: refreshed.expires_at,
            account_label: Some(active_label.clone()),
        });
    }

    Ok(refreshed.access_token)
}

/// Stream the response from Anthropic API
async fn stream_response(
    client: Client,
    token: String,
    is_oauth: bool,
    request: ApiRequest,
    tx: mpsc::Sender<Result<StreamEvent>>,
    model_name: &str,
    oauth_session_id: &str,
    direct_transport: &DirectTransportConfig,
) -> Result<()> {
    use jcode_message_types::ConnectionPhase;
    let requested_model_base = strip_1m_suffix(&request.model).to_ascii_lowercase();
    if std::env::var("JCODE_ANTHROPIC_DEBUG")
        .map(|v| v == "1")
        .unwrap_or(false)
        && let Ok(json) = serde_json::to_string_pretty(&request)
    {
        jcode_base::logging::info(&format!("Anthropic request payload:\n{}", json));
    }

    let _ = tx
        .send(Ok(StreamEvent::ConnectionPhase {
            phase: ConnectionPhase::SendingRequest,
        }))
        .await;

    let connect_start = std::time::Instant::now();
    let stream_idle_timeout = jcode_base::provider::stream_idle_timeout();
    // Build request with appropriate auth headers
    // Upstream picks the endpoint (OAuth vs the configurable direct base);
    // `api_url` then applies the test-only override when one is installed, so
    // the E2E rotation test can drive this exact code path against a mock.
    let url = if is_oauth {
        API_URL_OAUTH.to_string()
    } else {
        direct_transport.api_url.clone()
    };
    let url = test_override_url().unwrap_or(url);

    let mut req = client.post(url.as_str());
    if !is_oauth {
        req = req.headers(
            direct_transport
                .headers
                .clone()
                .map_err(anyhow::Error::msg)?,
        );
    }
    req = req
        .header("anthropic-version", API_VERSION)
        .header("content-type", "application/json")
        .header(
            "accept",
            if is_oauth {
                "application/json"
            } else {
                "text/event-stream"
            },
        );

    if is_oauth {
        // OAuth tokens require:
        // 1. Bearer auth (NOT x-api-key)
        // 2. User-Agent matching Claude CLI
        // 3. Multiple beta headers
        // 4. ?beta=true query param (in URL above)
        let beta_header = anthropic_beta_header_with_thinking(
            oauth_beta_headers(model_name),
            request.thinking.is_some(),
        );
        let beta_header = anthropic_beta_header_with_fast(&beta_header, request.speed.is_some());
        req = apply_oauth_attribution_headers(
            req.header("Authorization", format!("Bearer {}", token))
                .header("User-Agent", CLAUDE_CLI_USER_AGENT)
                .header("anthropic-beta", beta_header),
            oauth_session_id,
        );
    } else {
        // Direct API keys use x-api-key
        // Include prompt-caching beta header
        let beta_header = if is_1m_model(model_name) {
            "prompt-caching-2024-07-31,context-1m-2025-08-07"
        } else {
            "prompt-caching-2024-07-31"
        };
        let beta_header =
            anthropic_beta_header_with_thinking(beta_header, request.thinking.is_some());
        let beta_header = anthropic_beta_header_with_fast(&beta_header, request.speed.is_some());
        req = match direct_transport.auth_mode.as_str() {
            "none" => req,
            "bearer" => req.header("Authorization", format!("Bearer {token}")),
            "header" => {
                let header = HeaderName::from_bytes(direct_transport.auth_header.trim().as_bytes())
                    .context("invalid JCODE_ANTHROPIC_AUTH_HEADER")?;
                req.header(header, &token)
            }
            value => anyhow::bail!(
                "invalid JCODE_ANTHROPIC_AUTH value '{value}' (expected bearer, header, or none)"
            ),
        };
        req = req.header("anthropic-beta", beta_header);
    }

    let response = jcode_provider_core::transport::send_with_initial_response_timeout(
        req.json(&request),
        stream_idle_timeout,
    )
    .await
    .context("Failed to send request to Anthropic API")?;

    let connect_ms = connect_start.elapsed().as_millis();
    jcode_base::logging::info(&format!(
        "HTTP connection established in {}ms (status={})",
        connect_ms,
        response.status()
    ));

    if !response.status().is_success() {
        let status = response.status();
        let retry_after = jcode_provider_core::retry_after::retry_after(response.headers());
        let error_text = jcode_base::util::http_error_body(response, "HTTP error").await;
        return Err(jcode_provider_core::retry_after::error_with_retry_after(
            format!("Anthropic API error ({}): {}", status, error_text),
            retry_after,
        ));
    }

    let _ = tx
        .send(Ok(StreamEvent::ConnectionPhase {
            phase: ConnectionPhase::WaitingForResponse,
        }))
        .await;

    // Parse SSE stream
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut sse_state = SseStreamState {
        requested_model_base,
        ..SseStreamState::default()
    };

    // Idle timeout between streamed chunks. Configurable via
    // `[provider] stream_idle_timeout_secs` / `JCODE_STREAM_IDLE_TIMEOUT_SECS`
    // so slow reasoning models don't trip a premature timeout (issue #434).
    loop {
        let chunk = match tokio::time::timeout(stream_idle_timeout, stream.next()).await {
            Ok(Some(chunk_result)) => chunk_result.context("Error reading stream chunk")?,
            Ok(None) => break, // stream ended normally
            Err(_) => {
                jcode_base::logging::warn(&format!(
                    "Anthropic SSE stream timed out (no data for {}s)",
                    stream_idle_timeout.as_secs()
                ));
                anyhow::bail!(
                    "Stream read timeout: no data received for {} seconds",
                    stream_idle_timeout.as_secs()
                );
            }
        };
        let chunk_str = String::from_utf8_lossy(&chunk);
        buffer.push_str(&chunk_str);

        // Process complete SSE events
        while let Some(event) = parse_sse_event(&mut buffer) {
            let events = process_sse_event(&event, &mut sse_state, is_oauth);
            for stream_event in events {
                if let StreamEvent::Error { ref message, .. } = stream_event
                    && is_retryable_error(&message.to_lowercase())
                {
                    anyhow::bail!("Retryable stream error: {}", message);
                }
                if tx.send(Ok(stream_event)).await.is_err() {
                    return Ok(()); // Receiver dropped
                }
            }
        }
    }

    // Send final token usage if we have it
    if sse_state.input_tokens.is_some() || sse_state.output_tokens.is_some() {
        // Log cache usage for debugging
        if sse_state.cache_read_input_tokens.is_some()
            || sse_state.cache_creation_input_tokens.is_some()
        {
            jcode_base::logging::info(&format!(
                "Prompt cache: read={:?} created={:?}",
                sse_state.cache_read_input_tokens, sse_state.cache_creation_input_tokens
            ));
        }
        let _ = tx
            .send(Ok(StreamEvent::TokenUsage {
                input_tokens: sse_state.input_tokens,
                output_tokens: sse_state.output_tokens,
                cache_read_input_tokens: sse_state.cache_read_input_tokens,
                cache_creation_input_tokens: sse_state.cache_creation_input_tokens,
            }))
            .await;
    }

    Ok(())
}

/// Check if an error is transient and should be retried
fn is_retryable_error(error_str: &str) -> bool {
    jcode_provider_core::is_transient_transport_error(error_str)
        // Server errors (5xx)
        || error_str.contains("500 internal server error")
        || error_str.contains("502 bad gateway")
        || error_str.contains("503 service unavailable")
        || error_str.contains("504 gateway timeout")
        || error_str.contains("overloaded")
        // Rate limiting (429)
        || error_str.contains("429 too many requests")
        || error_str.contains("rate limit")
        || error_str.contains("rate_limit")
    // API-level server errors (SSE error events)
    || error_str.contains("api_error")
    || error_str.contains("internal server error")
}

/// Detect an Anthropic capacity rejection.
///
/// Anthropic signals "no capacity right now" as `overloaded_error` in an SSE
/// error event, or as HTTP 529 on the initial response. Both are retryable, but
/// they need a longer wait than a transport fault, so they are classified
/// separately from [`is_retryable_error`]. `error_str` is expected to already be
/// lowercased.
///
/// The 529 match is anchored on the surrounding status text that
/// `stream_response` emits, because a bare "529" also occurs inside request ids
/// and token counts.
fn is_overloaded_error(error_str: &str) -> bool {
    error_str.contains("overloaded") || error_str.contains("(529") || error_str.contains("529 ")
}

fn is_fable_scoped_limit_error(model: &str, error: &str) -> bool {
    let model = strip_1m_suffix(model).to_ascii_lowercase();
    if !model.contains("fable") {
        return false;
    }
    let error = error.to_ascii_lowercase();
    let is_limit = error.contains("rate_limit")
        || error.contains("rate limit")
        || error.contains("usage_limit")
        || error.contains("usage limit");
    let is_scoped = error.contains("fable")
        || error.contains("weekly")
        || error.contains("week limit")
        || error.contains("7-day")
        || error.contains("7 day");
    is_limit && is_scoped
}

/// Detect an Anthropic "model not found" rejection.
///
/// Anthropic returns HTTP 404 with `"type":"not_found_error"` when a model id
/// has been retired or renamed (e.g. `claude-fable-5` after it was folded into
/// Opus 4.8). The message text varies, so match on the stable structural
/// markers. `error_str` is expected to already be lowercased.
fn is_model_not_found_error(error_str: &str) -> bool {
    let mentions_model = error_str.contains("model");
    let is_not_found = error_str.contains("not_found_error")
        || error_str.contains("404 not found")
        || error_str.contains("(404 ");
    is_not_found
        && (mentions_model
            || error_str.contains("is not available")
            || error_str.contains("please use"))
}

/// Detect an Anthropic rejection of an explicit reasoning request.
///
/// Some models are listed with reasoning capabilities in `GET /v1/models` but
/// the live Messages API still rejects them with a 400 `invalid_request_error`:
///
/// - an adaptive `thinking` block -> "adaptive thinking is not supported on
///   this model"
/// - an `output_config` effort -> "This model does not support the effort
///   parameter."
///
/// (e.g. `claude-fable-5`). When we hit either we can self-heal by dropping the
/// offending reasoning fields and retrying, rather than hard-failing the turn.
/// `error_str` is expected to already be lowercased.
fn is_reasoning_unsupported_error(error_str: &str) -> bool {
    let is_bad_request =
        error_str.contains("invalid_request_error") || error_str.contains("400 bad request");
    let mentions_reasoning_field = error_str.contains("thinking")
        || error_str.contains("effort")
        || error_str.contains("output_config");
    let mentions_unsupported =
        error_str.contains("not supported") || error_str.contains("does not support");
    is_bad_request && mentions_reasoning_field && mentions_unsupported
}

/// True when Anthropic refused the request *because of fast mode* rather than
/// anything about the conversation, so retrying at standard speed will work.
///
/// Three documented causes, all reported as a 400 or a 429 naming fast mode:
/// the organization has not enabled it, the account has no usage credits, or
/// research-preview access is not provisioned. Observed live on 2026-08-27:
/// "Fast mode is not enabled for your organization. An organization admin must
/// enable this feature."
///
/// `error_str` is expected to already be lowercased.
fn is_fast_mode_unavailable_error(error_str: &str) -> bool {
    error_str.contains("fast mode")
        && (error_str.contains("not enabled")
            || error_str.contains("not available")
            || error_str.contains("unavailable")
            || error_str.contains("disabled")
            || error_str.contains("usage credits")
            || error_str.contains("purchase credits"))
}

/// Models that have been retired and must never be chosen as a fallback target
/// (the server 404s them, so picking one just loops). Matched as a substring of
/// the normalized id so dated variants are covered too. `claude-fable` was
/// briefly on this list while retired, but the model is live again.
const RETIRED_ANTHROPIC_MODEL_MARKERS: &[&str] = &["claude-mythos"];

fn anthropic_model_is_retired(model: &str) -> bool {
    let normalized = AnthropicProvider::normalized_model_key(model);
    RETIRED_ANTHROPIC_MODEL_MARKERS
        .iter()
        .any(|marker| normalized.contains(marker))
}

/// Quality rank for an Anthropic model id: lower is better. Uses the curated
/// flagship-first `ALL_CLAUDE_MODELS` order (Opus > Sonnet > Haiku > older), so
/// fallback never silently downgrades to a cheaper tier when a stronger model is
/// available. Unknown/uncurated ids sort after every curated one but before
/// retired models, which sort last.
fn anthropic_model_quality_rank(model: &str) -> usize {
    if anthropic_model_is_retired(model) {
        return usize::MAX;
    }
    let normalized = jcode_provider_core::model_id::strip_date_suffix(
        &jcode_provider_core::model_id::canonical(model),
    )
    .to_string();
    jcode_provider_core::ALL_CLAUDE_MODELS
        .iter()
        .position(|candidate| {
            jcode_provider_core::model_id::strip_date_suffix(
                &jcode_provider_core::model_id::canonical(candidate),
            ) == normalized
        })
        // Curated models keep their position; unknown-but-not-retired models sort
        // just after the curated list so they only win when nothing curated is
        // available.
        .unwrap_or(jcode_provider_core::ALL_CLAUDE_MODELS.len())
}

/// Parse a server-recommended replacement model from a 404 body, e.g.
/// "Claude Fable 5 is not available. Please use Opus 4.8." -> the catalog id
/// `claude-opus-4-8`. Returns the best matching known catalog id, if any.
/// `error_str` is expected to already be lowercased.
fn anthropic_recommended_model_from_error(error_str: &str) -> Option<String> {
    // Look for the phrase after "please use" / "use " and try to match it against
    // the known catalog by collapsing it to a comparable token form. The server
    // phrases the recommendation in prose ("Opus 4.8"), so compare on the digits
    // and family word rather than exact ids.
    let hint = error_str
        .split("please use")
        .nth(1)
        .or_else(|| error_str.split("use ").nth(1))?;
    // Take up to the next sentence boundary.
    let hint = hint.split(['.', '!', '\n']).next().unwrap_or(hint).trim();
    if hint.is_empty() {
        return None;
    }
    // Reduce the hint to alphanumeric tokens (e.g. "opus", "4", "8").
    let hint_tokens: Vec<String> = hint
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_ascii_lowercase())
        .collect();
    if hint_tokens.is_empty() {
        return None;
    }
    // Score each known catalog model by how many hint tokens it contains.
    jcode_base::provider::known_anthropic_model_ids()
        .into_iter()
        .filter(|candidate| !anthropic_model_is_retired(candidate))
        .map(|candidate| {
            let key = AnthropicProvider::normalized_model_key(&candidate);
            // The catalog id uses hyphenated digits ("claude-opus-4-8"), so the
            // hint tokens ["opus","4","8"] should all appear.
            let score = hint_tokens
                .iter()
                .filter(|token| key.contains(token.as_str()))
                .count();
            (candidate, score)
        })
        // Require at least the family word plus one version digit to match so we
        // do not pick an arbitrary model from a single shared token.
        .filter(|(_, score)| *score >= 2)
        .max_by_key(|(_, score)| *score)
        .map(|(candidate, _)| candidate)
}

/// Pick the next Anthropic model to try after a "model not found" failure.
///
/// Strategy (most authoritative first):
///   1. Honor any server "Please use X" recommendation parsed from the error.
///   2. Otherwise pick the highest-quality untried model from the curated
///      flagship-first catalog, skipping retired families so we never downgrade
///      to a cheaper tier (e.g. Haiku) while a stronger model is available.
///
/// Returns `None` once every viable candidate is exhausted so the caller can
/// surface the original error.
fn anthropic_fallback_model(tried: &[String], error_str: &str) -> Option<String> {
    let already_tried = |candidate: &str| {
        tried.iter().any(|model| {
            AnthropicProvider::normalized_model_key(model)
                == AnthropicProvider::normalized_model_key(candidate)
        })
    };

    // 1. Server recommendation wins when it points at an untried, non-retired
    //    model.
    if let Some(recommended) = anthropic_recommended_model_from_error(error_str)
        && !already_tried(&recommended)
        && !anthropic_model_is_retired(&recommended)
    {
        return Some(recommended);
    }

    // 2. Best available by curated quality order, skipping retired and tried.
    jcode_base::provider::known_anthropic_model_ids()
        .into_iter()
        .filter(|candidate| !already_tried(candidate) && !anthropic_model_is_retired(candidate))
        .min_by_key(|candidate| anthropic_model_quality_rank(candidate))
}

fn is_oauth_auth_error(error_str: &str) -> bool {
    error_str.contains("oauth token has expired")
        || error_str.contains("token has expired")
        || error_str.contains("authentication_error")
        || error_str.contains("invalid token")
        || error_str.contains("invalid_grant")
        || error_str.contains("does not meet scope requirement")
        || ((error_str.contains("401 unauthorized") || error_str.contains("403 forbidden"))
            && (error_str.contains("oauth") || error_str.contains("token")))
}

fn is_oauth_catalog_auth_error(error_str: &str) -> bool {
    let lower = error_str.to_ascii_lowercase();
    lower.contains("401 unauthorized")
        || lower.contains("403 forbidden")
        || is_oauth_auth_error(&lower)
}

fn anthropic_beta_header_with_thinking(base: &str, thinking_enabled: bool) -> String {
    if thinking_enabled && !base.contains("interleaved-thinking-2025-05-14") {
        format!("{base},interleaved-thinking-2025-05-14")
    } else {
        base.to_string()
    }
}

/// Append the fast-mode beta token when the request opts into `speed: "fast"`.
///
/// Anthropic rejects the `speed` field without this header, so the two must
/// always travel together.
fn anthropic_beta_header_with_fast(base: &str, fast: bool) -> String {
    if fast && !base.contains(jcode_provider_core::ANTHROPIC_FAST_MODE_BETA) {
        format!("{base},{}", jcode_provider_core::ANTHROPIC_FAST_MODE_BETA)
    } else {
        base.to_string()
    }
}

/// Accumulator for tool_use blocks (input comes in chunks)
struct ToolUseAccumulator {
    input_json: String,
}

/// Parse a single SSE event from the buffer
fn parse_sse_event(buffer: &mut String) -> Option<SseEvent> {
    // Look for complete event (ends with double newline)
    let event_end = buffer.find("\n\n")?;
    let event_str = buffer[..event_end].to_string();
    buffer.drain(..event_end + 2);

    let mut event_type = String::new();
    let mut data = String::new();

    for line in event_str.lines() {
        if let Some(rest) = line.strip_prefix("event: ") {
            event_type = rest.to_string();
        } else if let Some(rest) = jcode_base::util::sse_data_line(line) {
            data = rest.to_string();
        }
    }

    if event_type.is_empty() && data.is_empty() {
        return None;
    }

    Some(SseEvent { event_type, data })
}

/// SSE event from the stream
struct SseEvent {
    event_type: String,
    data: String,
}

/// Mutable accumulator state threaded through [`process_sse_event`] across a
/// single SSE response stream.
#[derive(Default)]
struct SseStreamState {
    current_tool_use: Option<ToolUseAccumulator>,
    current_thinking_block: bool,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    /// Lowercased base id of the model we asked for, so `message_start` can flag
    /// a silent server-side substitution (e.g. an unavailable id aliased to a
    /// different model). Empty when unknown (e.g. in unit tests).
    requested_model_base: String,
    /// Set once we have warned about a substitution, so we only warn per stream.
    warned_model_substitution: bool,
}

/// Process an SSE event and return StreamEvents if applicable
fn process_sse_event(
    event: &SseEvent,
    state: &mut SseStreamState,
    is_oauth: bool,
) -> Vec<StreamEvent> {
    let mut events = Vec::new();

    match event.event_type.as_str() {
        "message_start" => {
            // Extract usage from message_start (includes cache info)
            if let Ok(parsed) = serde_json::from_str::<MessageStartEvent>(&event.data) {
                // The server echoes the model that actually served the request.
                // Log it so we can confirm there was no silent server-side
                // substitution (and surface it under JCODE_LOG_SERVED_MODEL).
                if let Some(served) = parsed.message.model.as_deref() {
                    jcode_base::logging::info(&format!("Anthropic served model={}", served));
                    if std::env::var("JCODE_LOG_SERVED_MODEL").is_ok() {
                        eprintln!("[anthropic] served model={served}");
                    }
                    // Anthropic can silently alias an unavailable/retired model
                    // id to a different model (observed: claude-fable-5 ->
                    // claude-haiku-4-5). That is a correctness hazard: the user
                    // believes they are on the requested flagship. Warn loudly
                    // once per stream when the served base id differs.
                    let served_base = strip_1m_suffix(served).to_ascii_lowercase();
                    if !state.requested_model_base.is_empty()
                        && !state.warned_model_substitution
                        && served_base != state.requested_model_base
                    {
                        state.warned_model_substitution = true;
                        jcode_base::logging::warn(&format!(
                            "Anthropic served a DIFFERENT model than requested: requested '{}', served '{}'. The requested model is likely unavailable and is being substituted server-side.",
                            state.requested_model_base, served_base
                        ));
                        events.push(StreamEvent::StatusDetail {
                            detail: format!(
                                "⚠ Anthropic served '{}' instead of requested '{}' (requested model unavailable)",
                                served_base, state.requested_model_base
                            ),
                        });
                    }
                }
                if let Some(usage) = parsed.message.usage {
                    state.input_tokens = usage.input_tokens.map(|t| t as u64);
                    state.cache_read_input_tokens = usage.cache_read_input_tokens.map(|t| t as u64);
                    state.cache_creation_input_tokens =
                        usage.cache_creation_input_tokens.map(|t| t as u64);
                    if let Some(tier) = usage.service_tier.as_deref() {
                        jcode_base::logging::info(&format!(
                            "Anthropic granted service_tier={}",
                            tier
                        ));
                        if std::env::var("JCODE_LOG_SERVICE_TIER").is_ok() {
                            eprintln!("[anthropic] granted service_tier={tier}");
                        }
                    }
                    if let Some(speed) = usage.speed.as_deref() {
                        jcode_base::logging::info(&format!("Anthropic granted speed={speed}"));
                        if std::env::var("JCODE_LOG_SERVICE_TIER").is_ok() {
                            eprintln!("[anthropic] granted speed={speed}");
                        }
                    }
                }
            }
        }
        "content_block_start" => {
            if let Ok(parsed) = serde_json::from_str::<ContentBlockStartEvent>(&event.data) {
                match parsed.content_block {
                    ApiContentBlockStart::Text { .. } => {
                        // Text block starting - nothing to emit yet
                    }
                    ApiContentBlockStart::Thinking { _thinking, .. } => {
                        state.current_thinking_block = true;
                        events.push(StreamEvent::ThinkingStart);
                        if !_thinking.is_empty() {
                            events.push(StreamEvent::ThinkingDelta(_thinking));
                        }
                    }
                    ApiContentBlockStart::RedactedThinking { .. } => {
                        state.current_thinking_block = true;
                        events.push(StreamEvent::ThinkingStart);
                    }
                    ApiContentBlockStart::ToolUse { id, name } => {
                        let mapped_name = if is_oauth {
                            map_tool_name_from_oauth(&name)
                        } else {
                            name.clone()
                        };
                        // Start accumulating tool use
                        state.current_tool_use = Some(ToolUseAccumulator {
                            input_json: String::new(),
                        });
                        events.push(StreamEvent::ToolUseStart {
                            id,
                            name: mapped_name,
                        });
                    }
                    ApiContentBlockStart::Unknown => {
                        // Newer/unsupported block type. Parsing succeeded, so
                        // the rest of the stream stays intact; there is simply
                        // nothing for this build to surface.
                        jcode_base::logging::warn(
                            "Anthropic stream sent an unrecognized content_block_start type; ignoring the block",
                        );
                    }
                }
            }
        }
        "content_block_delta" => {
            if let Ok(parsed) = serde_json::from_str::<ContentBlockDeltaEvent>(&event.data) {
                match parsed.delta {
                    ApiDelta::Text { text } => {
                        events.push(StreamEvent::TextDelta(text));
                    }
                    ApiDelta::InputJson { partial_json } => {
                        if let Some(tool) = state.current_tool_use.as_mut() {
                            tool.input_json.push_str(&partial_json);
                        }
                        events.push(StreamEvent::ToolInputDelta(partial_json));
                    }
                    ApiDelta::Thinking { thinking } => {
                        events.push(StreamEvent::ThinkingDelta(thinking));
                    }
                    ApiDelta::Signature { signature } => {
                        events.push(StreamEvent::ThinkingSignatureDelta(signature));
                    }
                }
            }
        }
        "content_block_stop" => {
            // If we were accumulating a tool_use, it's complete now
            if state.current_tool_use.take().is_some() {
                events.push(StreamEvent::ToolUseEnd);
            } else if state.current_thinking_block {
                state.current_thinking_block = false;
                events.push(StreamEvent::ThinkingEnd);
            }
        }
        "message_delta" => {
            if let Ok(parsed) = serde_json::from_str::<MessageDeltaEvent>(&event.data) {
                if let Some(usage) = parsed.usage {
                    state.output_tokens = usage.output_tokens.map(|t| t as u64);
                }
                if let Some(stop_reason) = parsed.delta.stop_reason {
                    events.push(StreamEvent::MessageEnd {
                        stop_reason: Some(stop_reason),
                    });
                }
            }
        }
        "message_stop" => {
            // Final message stop - we may have already sent MessageEnd via message_delta
        }
        "ping" => {
            // Keepalive. Surface it as a phase event instead of swallowing it:
            // during silent reasoning phases (adaptive thinking with hidden or
            // summarized display) pings can be the only upstream traffic, and
            // downstream consumers (the TUI stall guard) need to see *some*
            // event to know the stream is alive (issue #451).
            events.push(StreamEvent::ConnectionPhase {
                phase: jcode_message_types::ConnectionPhase::Streaming,
            });
        }
        "error" => {
            jcode_base::logging::error(&format!("Anthropic stream error: {}", event.data));
            events.push(StreamEvent::Error {
                message: event.data.clone(),
                retry_after_secs: None,
            });
        }
        _ => {
            // Unknown event type, ignore
        }
    }

    events
}

// ============================================================================
// API Types
// ============================================================================

fn build_system_param(system: &str, is_oauth: bool) -> Option<ApiSystem> {
    jcode_provider_anthropic::build_system_param(system, is_oauth, is_cache_ttl_1h())
}

fn build_system_param_split(
    static_part: &str,
    dynamic_part: &str,
    is_oauth: bool,
) -> Option<ApiSystem> {
    jcode_provider_anthropic::build_system_param_split(
        static_part,
        dynamic_part,
        is_oauth,
        is_cache_ttl_1h(),
    )
}

fn format_messages_with_identity(messages: Vec<ApiMessage>, is_oauth: bool) -> Vec<ApiMessage> {
    jcode_provider_anthropic::format_messages_with_identity(messages, is_oauth, is_cache_ttl_1h())
}

#[cfg(test)]
fn add_message_cache_breakpoint(messages: &mut [ApiMessage]) {
    jcode_provider_anthropic::add_message_cache_breakpoint(messages, is_cache_ttl_1h())
}

mod sse_types;
use sse_types::{
    ApiContentBlockStart, ApiDelta, ContentBlockDeltaEvent, ContentBlockStartEvent,
    MessageDeltaEvent, MessageStartEvent,
};

mod context_window;

#[cfg(test)]
#[allow(clippy::await_holding_lock)]
#[path = "anthropic_tests.rs"]
mod tests;
