use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use anyhow::Result;
use serde_json::{Value, json};

use crate::tokens::{load_all_tokens, save_token};
use crate::types::{
    AvailableAccount, ProviderId, RefreshTokenExhaustedError, TokenData, UsageData,
};
use crate::utils::{now_iso, sha256_hex};

pub type RefreshFuture = Pin<Box<dyn Future<Output = Result<TokenData>> + Send>>;
pub type RefreshFn = Box<dyn Fn(String) -> RefreshFuture + Send + Sync>;

const RATE_LIMIT_BACKOFF: (f64, f64) = (60.0, 15.0 * 60.0);
const AUTH_BACKOFF: (f64, f64) = (10.0 * 60.0, 60.0 * 60.0);
const SERVER_BACKOFF: (f64, f64) = (5.0, 5.0 * 60.0);
const REAUTH_COOLDOWN_SECONDS: f64 = 24.0 * 60.0 * 60.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshPolicyKind {
    ExpiresLead,
    SinceLastRefresh,
    /// Never refresh — for static credentials (e.g. opencode-go API keys) that cannot expire.
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefreshPolicy {
    pub kind: RefreshPolicyKind,
    pub seconds: i64,
}

#[derive(Debug, Clone)]
pub struct AccountResult {
    pub account: Option<AvailableAccount>,
    pub failure_kind: Option<String>,
    pub retry_after_seconds: Option<f64>,
}

impl Default for RefreshPolicy {
    fn default() -> Self {
        Self {
            kind: RefreshPolicyKind::ExpiresLead,
            seconds: 4 * 60 * 60,
        }
    }
}

#[derive(Debug, Clone)]
struct AccountState {
    token: TokenData,
    cooldown_until: f64,
    failure_count: i64,
    last_failure_kind: Option<String>,
    last_error: Option<String>,
    last_failure_at: Option<String>,
    last_success_at: Option<String>,
    last_refresh_at: Option<String>,
    total_requests: i64,
    total_successes: i64,
    total_failures: i64,
    total_input_tokens: i64,
    total_output_tokens: i64,
    total_cache_creation_input_tokens: i64,
    total_cache_read_input_tokens: i64,
    total_reasoning_output_tokens: i64,
}

impl AccountState {
    fn new(token: TokenData) -> Self {
        let last_refresh_at = token.last_refresh_at.clone();
        Self {
            token,
            cooldown_until: 0.0,
            failure_count: 0,
            last_failure_kind: None,
            last_error: None,
            last_failure_at: None,
            last_success_at: None,
            last_refresh_at,
            total_requests: 0,
            total_successes: 0,
            total_failures: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_creation_input_tokens: 0,
            total_cache_read_input_tokens: 0,
            total_reasoning_output_tokens: 0,
        }
    }
}

pub struct AccountManager {
    auth_dir: PathBuf,
    provider: ProviderId,
    refresh: RefreshFn,
    refresh_policy: RefreshPolicy,
    accounts: BTreeMap<String, AccountState>,
    order: Vec<String>,
    last_used_index: Option<usize>,
}

impl AccountManager {
    #[must_use]
    pub fn new(
        auth_dir: PathBuf,
        provider: ProviderId,
        refresh: impl Fn(String) -> RefreshFuture + Send + Sync + 'static,
        refresh_policy: RefreshPolicy,
    ) -> Self {
        Self {
            auth_dir,
            provider,
            refresh: Box::new(refresh),
            refresh_policy,
            accounts: BTreeMap::new(),
            order: Vec::new(),
            last_used_index: None,
        }
    }

    #[must_use]
    pub fn account_count(&self) -> usize {
        self.accounts.len()
    }

    /// Load provider token files from disk.
    ///
    /// # Errors
    ///
    /// Returns an error when the auth directory exists but cannot be read.
    pub fn load(&mut self) -> Result<()> {
        for token in load_all_tokens(&self.auth_dir, Some(self.provider))? {
            self.upsert_loaded_token(token);
        }
        Ok(())
    }

    /// Reload provider token files from disk and report changed account emails.
    ///
    /// # Errors
    ///
    /// Returns an error when the auth directory exists but cannot be read.
    pub fn reload(&mut self) -> Result<Value> {
        let mut added = Vec::new();
        let mut updated = Vec::new();
        let mut unchanged = Vec::new();
        for token in load_all_tokens(&self.auth_dir, Some(self.provider))? {
            let Some(existing) = self.accounts.get_mut(&token.email) else {
                added.push(token.email.clone());
                self.upsert_loaded_token(token);
                continue;
            };
            if existing.token.access_token == token.access_token
                && existing.token.refresh_token == token.refresh_token
            {
                unchanged.push(token.email.clone());
                continue;
            }
            updated.push(token.email.clone());
            existing.token = token;
            existing.cooldown_until = 0.0;
            existing.failure_count = 0;
            existing.last_failure_kind = None;
            existing.last_error = None;
            existing.last_failure_at = None;
        }
        Ok(json!({
            "added": added,
            "updated": updated,
            "unchanged": unchanged
        }))
    }

    /// Refresh an account when its configured refresh policy says it is due.
    ///
    /// # Errors
    ///
    /// Returns an error when the refresh callback fails or refreshed token persistence fails.
    pub async fn refresh_if_due(&mut self, email: &str) -> Result<bool> {
        if !self
            .accounts
            .get(email)
            .is_some_and(|state| self.should_refresh(state))
        {
            return Ok(true);
        }
        self.refresh_account(email).await
    }

    /// Force-refresh one account.
    ///
    /// # Errors
    ///
    /// Returns an error when the refresh callback fails or refreshed token persistence fails.
    pub async fn refresh_account(&mut self, email: &str) -> Result<bool> {
        let Some(state) = self.accounts.get(email) else {
            return Ok(false);
        };
        let old_token = state.token.clone();
        let refreshed = match (self.refresh)(old_token.refresh_token.clone()).await {
            Ok(token) => token,
            Err(error) => {
                if let Some(exhausted) = error.downcast_ref::<RefreshTokenExhaustedError>() {
                    self.record_refresh_exhausted(email, &exhausted.reason);
                    return Ok(false);
                }
                return Err(error);
            }
        };
        let refresh_at = now_iso();
        let new_token = TokenData {
            access_token: refreshed.access_token,
            refresh_token: refreshed.refresh_token,
            email: if refreshed.email.is_empty() {
                old_token.email.clone()
            } else {
                refreshed.email
            },
            expires_at: refreshed.expires_at,
            account_uuid: if refreshed.account_uuid.is_empty() {
                old_token.account_uuid.clone()
            } else {
                refreshed.account_uuid
            },
            provider: self.provider,
            id_token: refreshed.id_token.or(old_token.id_token),
            last_refresh_at: Some(refresh_at.clone()),
            plan_type: refreshed.plan_type.or(old_token.plan_type),
            cursor: refreshed.cursor.or(old_token.cursor),
        };
        save_token(&self.auth_dir, &new_token)?;
        if let Some(state) = self.accounts.get_mut(email) {
            state.token = new_token;
            state.cooldown_until = 0.0;
            state.failure_count = 0;
            state.last_failure_kind = None;
            state.last_error = None;
            state.last_failure_at = None;
            state.last_success_at = Some(refresh_at.clone());
            state.last_refresh_at = Some(refresh_at);
        }
        Ok(true)
    }

    pub fn record_success(&mut self, email: &str, usage: Option<&UsageData>) {
        let Some(state) = self.accounts.get_mut(email) else {
            return;
        };
        state.cooldown_until = 0.0;
        state.failure_count = 0;
        state.last_failure_kind = None;
        state.last_error = None;
        state.last_failure_at = None;
        state.last_success_at = Some(now_iso());
        state.total_successes += 1;
        if let Some(usage) = usage {
            state.total_input_tokens += usage.input_tokens;
            state.total_output_tokens += usage.output_tokens;
            state.total_cache_creation_input_tokens += usage.cache_creation_input_tokens;
            state.total_cache_read_input_tokens += usage.cache_read_input_tokens;
            state.total_reasoning_output_tokens += usage.reasoning_output_tokens;
        }
    }

    pub fn record_attempt(&mut self, email: &str) {
        if let Some(state) = self.accounts.get_mut(email) {
            state.total_requests += 1;
        }
    }

    pub fn record_failure(&mut self, email: &str, kind: &str, detail: Option<&str>) {
        let Some(state) = self.accounts.get_mut(email) else {
            return;
        };
        state.failure_count += 1;
        state.total_failures += 1;
        state.last_failure_kind = Some(kind.to_string());
        state.last_failure_at = Some(now_iso());
        state.last_error =
            Some(detail.map_or_else(|| kind.to_string(), |detail| format!("{kind}: {detail}")));
        let (base, maximum) = failure_backoff(kind);
        let multiplier = 2_f64.powi(i32::try_from(state.failure_count - 1).unwrap_or(0));
        state.cooldown_until = unix_now() + (base * multiplier).min(maximum);
    }

    pub fn record_refresh_exhausted(&mut self, email: &str, reason: &str) {
        let Some(state) = self.accounts.get_mut(email) else {
            return;
        };
        state.failure_count += 1;
        state.total_failures += 1;
        state.last_failure_kind = Some("auth".to_string());
        state.last_failure_at = Some(now_iso());
        state.last_error = Some(format!(
            "refresh token {reason}; re-run login for {}",
            self.provider
        ));
        state.cooldown_until = unix_now() + REAUTH_COOLDOWN_SECONDS;
    }

    #[must_use]
    pub fn snapshots(&self) -> Vec<Value> {
        let now = unix_now();
        self.accounts
            .values()
            .map(|state| {
                let cooldown_remaining = (state.cooldown_until - now).max(0.0);
                json!({
                    "email": state.token.email,
                    "available": cooldown_remaining == 0.0,
                    "cooldownUntil": if cooldown_remaining == 0.0 { 0.0 } else { state.cooldown_until },
                    "failureCount": state.failure_count,
                    "lastError": state.last_error,
                    "lastFailureAt": state.last_failure_at,
                    "lastSuccessAt": state.last_success_at,
                    "lastRefreshAt": state.last_refresh_at,
                    "totalRequests": state.total_requests,
                    "totalSuccesses": state.total_successes,
                    "totalFailures": state.total_failures,
                    "totalInputTokens": state.total_input_tokens,
                    "totalOutputTokens": state.total_output_tokens,
                    "totalCacheCreationInputTokens": state.total_cache_creation_input_tokens,
                    "totalCacheReadInputTokens": state.total_cache_read_input_tokens,
                    "totalReasoningOutputTokens": state.total_reasoning_output_tokens,
                    "expiresAt": state.token.expires_at,
                    "refreshing": false,
                    "planType": state.token.plan_type
                })
            })
            .collect()
    }

    #[must_use]
    pub fn next_account(&mut self) -> Option<AvailableAccount> {
        self.next_account_result().account
    }

    #[must_use]
    pub fn next_account_result(&mut self) -> AccountResult {
        if self.order.is_empty() {
            return AccountResult {
                account: None,
                failure_kind: None,
                retry_after_seconds: None,
            };
        }
        let now = unix_now();
        let start = self.last_used_index.map_or(0, |index| index + 1);
        for offset in 0..self.order.len() {
            let index = (start + offset) % self.order.len();
            let email = &self.order[index];
            let state = &self.accounts[email];
            if state.cooldown_until <= now {
                self.last_used_index = Some(index);
                return AccountResult {
                    account: Some(self.available_account(state)),
                    failure_kind: None,
                    retry_after_seconds: None,
                };
            }
        }
        let best = self
            .order
            .iter()
            .filter_map(|email| self.accounts.get(email))
            .min_by(|left, right| {
                let left_remaining = left.cooldown_until - now;
                let right_remaining = right.cooldown_until - now;
                left_remaining.total_cmp(&right_remaining)
            });
        AccountResult {
            account: None,
            failure_kind: best.and_then(|state| state.last_failure_kind.clone()),
            retry_after_seconds: best.map(|state| (state.cooldown_until - now).max(0.0)),
        }
    }

    #[must_use]
    pub fn account(&self, email: &str) -> Option<AvailableAccount> {
        self.accounts
            .get(email)
            .map(|state| self.available_account(state))
    }

    fn should_refresh(&self, state: &AccountState) -> bool {
        match self.refresh_policy.kind {
            RefreshPolicyKind::Never => false,
            RefreshPolicyKind::SinceLastRefresh => {
                let Some(last_refresh_at) = &state.last_refresh_at else {
                    return true;
                };
                let Ok(last_refresh_at) = chrono::DateTime::parse_from_rfc3339(last_refresh_at)
                else {
                    return true;
                };
                chrono::Utc::now()
                    .signed_duration_since(last_refresh_at.with_timezone(&chrono::Utc))
                    .num_seconds()
                    >= self.refresh_policy.seconds
            }
            RefreshPolicyKind::ExpiresLead => {
                let Ok(expires_at) = chrono::DateTime::parse_from_rfc3339(&state.token.expires_at)
                else {
                    return true;
                };
                expires_at
                    .with_timezone(&chrono::Utc)
                    .signed_duration_since(chrono::Utc::now())
                    .num_seconds()
                    <= self.refresh_policy.seconds
            }
        }
    }

    fn upsert_loaded_token(&mut self, token: TokenData) {
        let email = token.email.clone();
        if self.accounts.contains_key(&email) {
            self.accounts
                .get_mut(&email)
                .expect("account exists after contains_key")
                .token = token;
        } else {
            self.order.push(email.clone());
            self.accounts.insert(email, AccountState::new(token));
        }
    }

    fn available_account(&self, state: &AccountState) -> AvailableAccount {
        AvailableAccount {
            token: state.token.clone(),
            device_id: sha256_hex(&format!(
                "{}:{}",
                self.auth_dir.display(),
                state.token.email
            ))[..32]
                .to_string(),
            account_uuid: state.token.account_uuid.clone(),
            provider: self.provider,
            chatgpt_account_id: (self.provider == ProviderId::Codex)
                .then(|| state.token.account_uuid.clone()),
        }
    }
}

fn failure_backoff(kind: &str) -> (f64, f64) {
    match kind {
        "rate_limit" => RATE_LIMIT_BACKOFF,
        "auth" | "forbidden" => AUTH_BACKOFF,
        _ => SERVER_BACKOFF,
    }
}

fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |duration| duration.as_secs_f64())
}
