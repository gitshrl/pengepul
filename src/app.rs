use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use async_stream::try_stream;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::{Stream, StreamExt, TryStreamExt};
use serde_json::{Value, json};
use tower_http::cors::{Any, CorsLayer};

use crate::accounts::{AccountManager, RefreshPolicy, RefreshPolicyKind};
use crate::config::Config;
use crate::oauth::{refresh_anthropic_tokens, refresh_codex_tokens};
use crate::providers::{ProviderRegistry, build_registry};
use crate::streaming::{
    AnthropicStreamState, ChatStreamState, ResponsesStreamState, anthropic_sse_to_chat,
    anthropic_sse_to_responses, drain_complete_sse_events, finish_sse_events,
    responses_sse_to_anthropic, responses_sse_to_chat, responses_sse_to_payload, sse,
};
use crate::translate::{
    anthropic_to_openai, anthropic_to_responses, anthropic_to_responses_request,
    chat_to_responses_request, openai_to_anthropic, resolve_model, responses_to_anthropic,
    responses_to_anthropic_message, responses_to_chat_completion,
};
use crate::types::{AvailableAccount, ProviderId, UsageData};
use crate::upstream::{
    ANTHROPIC_BASE_URL, CODEX_BASE_URL, CODEX_RESPONSES_PATH, anthropic_headers, apply_cloaking,
    codex_headers, normalize_codex_responses_body,
};
use crate::utils::now_iso;

const RATE_LIMIT_WINDOW: Duration = Duration::from_mins(1);
const RATE_LIMIT_MAX: u32 = 60;

pub type UpstreamFuture =
    Pin<Box<dyn Future<Output = anyhow::Result<UpstreamJsonResponse>> + Send>>;
pub type UpstreamSseStream = Pin<Box<dyn Stream<Item = anyhow::Result<Bytes>> + Send>>;
pub type UpstreamSseFuture =
    Pin<Box<dyn Future<Output = anyhow::Result<UpstreamSseResponse>> + Send>>;

#[derive(Debug, Clone)]
pub struct UpstreamRequest {
    pub body: Value,
    pub request_headers: BTreeMap<String, String>,
    pub account: AvailableAccount,
    pub config: Config,
}

#[derive(Debug, Clone)]
pub struct UpstreamJsonResponse {
    pub status: StatusCode,
    pub body: Value,
}

pub struct UpstreamSseResponse {
    pub status: StatusCode,
    pub body: UpstreamSseStream,
}

pub trait UpstreamClient: Send + Sync {
    fn anthropic_messages(&self, request: UpstreamRequest) -> UpstreamFuture;
    fn anthropic_messages_stream(&self, request: UpstreamRequest) -> UpstreamSseFuture;
    fn anthropic_count_tokens(&self, request: UpstreamRequest) -> UpstreamFuture;
    fn codex_responses(&self, request: UpstreamRequest) -> UpstreamFuture;
    fn codex_responses_stream(&self, request: UpstreamRequest) -> UpstreamSseFuture;
}

#[derive(Clone)]
struct AppState {
    config: Arc<Config>,
    registry: Arc<ProviderRegistry>,
    body_limit: BodyLimit,
    upstream: Arc<dyn UpstreamClient>,
    account_managers: Arc<AccountManagers>,
    rate_limit_buckets: Arc<StdMutex<BTreeMap<String, RateLimitBucket>>>,
}

struct AccountManagers {
    anthropic: tokio::sync::Mutex<AccountManager>,
    codex: tokio::sync::Mutex<AccountManager>,
}

#[derive(Clone)]
struct StreamAccounting {
    state: AppState,
    provider: ProviderId,
    account: AvailableAccount,
}

#[derive(Debug, Clone)]
enum BodyLimit {
    Unlimited,
    Limited(u64),
    Invalid,
}

#[derive(Debug, Clone)]
struct RateLimitBucket {
    count: u32,
    reset_at: Instant,
}

#[derive(Debug, Clone)]
struct AppError {
    status: StatusCode,
    message: String,
    error_type: Option<&'static str>,
    provider: Option<ProviderId>,
}

impl AppError {
    fn simple(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            error_type: None,
            provider: None,
        }
    }

    fn provider(
        status: StatusCode,
        message: impl Into<String>,
        error_type: &'static str,
        provider: ProviderId,
    ) -> Self {
        Self {
            status,
            message: message.into(),
            error_type: Some(error_type),
            provider: Some(provider),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let mut error = serde_json::Map::new();
        error.insert("message".to_string(), Value::String(self.message));
        if let Some(error_type) = self.error_type {
            error.insert("type".to_string(), Value::String(error_type.to_string()));
        }
        if let Some(provider) = self.provider {
            error.insert("provider".to_string(), Value::String(provider.to_string()));
        }
        (self.status, Json(json!({"error": error}))).into_response()
    }
}

#[derive(Debug, Clone, Default)]
struct HttpUpstreamClient {
    client: reqwest::Client,
}

impl UpstreamClient for HttpUpstreamClient {
    fn anthropic_messages(&self, request: UpstreamRequest) -> UpstreamFuture {
        let client = self.client.clone();
        Box::pin(async move {
            let stream = request
                .body
                .get("stream")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let timeout_ms = if stream {
                request.config.timeouts.stream_messages_ms
            } else {
                request.config.timeouts.messages_ms
            };
            let model = request
                .body
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or("claude-sonnet-4-6");
            let body = apply_cloaking(
                &request.body,
                &request.request_headers,
                &request.account,
                &request.config,
            );
            let headers = anthropic_headers(
                &request.account.token.access_token,
                stream,
                timeout_ms,
                model,
                &request.config,
                &request.request_headers,
                false,
            );
            send_json(
                client,
                format!("{ANTHROPIC_BASE_URL}/v1/messages?beta=true"),
                headers,
                body,
                timeout_ms,
            )
            .await
        })
    }

    fn anthropic_messages_stream(&self, request: UpstreamRequest) -> UpstreamSseFuture {
        let client = self.client.clone();
        Box::pin(async move {
            let timeout_ms = request.config.timeouts.stream_messages_ms;
            let model = request
                .body
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or("claude-sonnet-4-6");
            let body = apply_cloaking(
                &request.body,
                &request.request_headers,
                &request.account,
                &request.config,
            );
            let headers = anthropic_headers(
                &request.account.token.access_token,
                true,
                timeout_ms,
                model,
                &request.config,
                &request.request_headers,
                false,
            );
            send_stream(
                client,
                format!("{ANTHROPIC_BASE_URL}/v1/messages?beta=true"),
                headers,
                body,
                timeout_ms,
            )
            .await
        })
    }

    fn anthropic_count_tokens(&self, request: UpstreamRequest) -> UpstreamFuture {
        let client = self.client.clone();
        Box::pin(async move {
            let model = request
                .body
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or("claude-sonnet-4-6");
            let headers = anthropic_headers(
                &request.account.token.access_token,
                false,
                request.config.timeouts.count_tokens_ms,
                model,
                &request.config,
                &request.request_headers,
                false,
            );
            send_json(
                client,
                format!("{ANTHROPIC_BASE_URL}/v1/messages/count_tokens?beta=true"),
                headers,
                request.body,
                request.config.timeouts.count_tokens_ms,
            )
            .await
        })
    }

    fn codex_responses(&self, request: UpstreamRequest) -> UpstreamFuture {
        let client = self.client.clone();
        Box::pin(async move {
            let stream = request
                .body
                .get("stream")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let timeout_ms = if stream {
                request.config.timeouts.stream_messages_ms
            } else {
                request.config.timeouts.messages_ms
            };
            send_json(
                client,
                format!("{CODEX_BASE_URL}{CODEX_RESPONSES_PATH}"),
                codex_headers(&request.account, stream, &request.config),
                request.body,
                timeout_ms,
            )
            .await
        })
    }

    fn codex_responses_stream(&self, request: UpstreamRequest) -> UpstreamSseFuture {
        let client = self.client.clone();
        Box::pin(async move {
            send_stream(
                client,
                format!("{CODEX_BASE_URL}{CODEX_RESPONSES_PATH}"),
                codex_headers(&request.account, true, &request.config),
                request.body,
                request.config.timeouts.stream_messages_ms,
            )
            .await
        })
    }
}

#[derive(Debug, Clone, Copy)]
enum RequestRoute {
    Chat,
    Responses,
    Messages,
}

pub fn create_app(config: Config) -> Router {
    create_app_with_upstream(config, Arc::new(HttpUpstreamClient::default()))
}

pub fn create_app_with_upstream(config: Config, upstream: Arc<dyn UpstreamClient>) -> Router {
    let registry = build_registry(&config.auth_dir);
    let body_limit = parse_body_limit(&config.body_limit);
    let account_managers = build_account_managers(&config);
    let state = AppState {
        config: Arc::new(config),
        registry: Arc::new(registry),
        body_limit,
        upstream,
        account_managers: Arc::new(account_managers),
        rate_limit_buckets: Arc::new(StdMutex::new(BTreeMap::new())),
    };

    Router::new()
        .route("/health", get(health))
        .route("/admin/accounts", get(admin_accounts))
        .route("/admin/reload", post(admin_reload))
        .route("/v1/models", get(models))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/responses", post(responses))
        .route("/v1/messages", post(messages))
        .route("/v1/messages/count_tokens", post(count_tokens))
        .with_state(state)
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
                .allow_headers(Any),
        )
}

fn build_account_managers(config: &Config) -> AccountManagers {
    let mut anthropic = AccountManager::new(
        config.auth_dir.clone(),
        ProviderId::Anthropic,
        |refresh_token| Box::pin(refresh_anthropic_tokens(refresh_token)),
        RefreshPolicy::default(),
    );
    let mut codex = AccountManager::new(
        config.auth_dir.clone(),
        ProviderId::Codex,
        |refresh_token| Box::pin(refresh_codex_tokens(refresh_token)),
        RefreshPolicy {
            kind: RefreshPolicyKind::SinceLastRefresh,
            seconds: 8 * 24 * 60 * 60,
        },
    );
    let _ = anthropic.load();
    let _ = codex.load();
    AccountManagers {
        anthropic: tokio::sync::Mutex::new(anthropic),
        codex: tokio::sync::Mutex::new(codex),
    }
}

async fn health() -> Json<Value> {
    Json(json!({"status": "ok"}))
}

async fn admin_accounts(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(error) = require_api_key(&state, &headers, false) {
        return error.into_response();
    }

    let anthropic = state.account_managers.anthropic.lock().await;
    let codex = state.account_managers.codex.lock().await;
    let providers = serde_json::Map::from_iter([
        (
            ProviderId::Anthropic.to_string(),
            json!({
                "accounts": anthropic.snapshots(),
                "account_count": anthropic.account_count()
            }),
        ),
        (
            ProviderId::Codex.to_string(),
            json!({
                "accounts": codex.snapshots(),
                "account_count": codex.account_count()
            }),
        ),
    ]);

    Json(json!({"providers": providers, "generated_at": now_iso()})).into_response()
}

async fn admin_reload(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(error) = require_api_key(&state, &headers, false) {
        return error.into_response();
    }

    let anthropic = state.account_managers.anthropic.lock().await.reload();
    let codex = state.account_managers.codex.lock().await.reload();
    let reloaded = match (anthropic, codex) {
        (Ok(anthropic), Ok(codex)) => serde_json::Map::from_iter([
            (ProviderId::Anthropic.to_string(), anthropic),
            (ProviderId::Codex.to_string(), codex),
        ]),
        (Err(error), _) | (_, Err(error)) => {
            return AppError::simple(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to reload accounts: {error}"),
            )
            .into_response();
        }
    };

    Json(json!({"reloaded": reloaded, "generated_at": now_iso()})).into_response()
}

async fn models(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(error) = require_api_key(&state, &headers, true) {
        return error.into_response();
    }

    let created = chrono::Utc::now().timestamp();
    let has_anthropic = state
        .account_managers
        .anthropic
        .lock()
        .await
        .account_count()
        > 0;
    let has_codex = state.account_managers.codex.lock().await.account_count() > 0;
    let models = [
        (ProviderId::Anthropic, "claude-sonnet-4-6"),
        (ProviderId::Anthropic, "claude-opus-4-8"),
        (ProviderId::Codex, "gpt-5.4"),
    ]
    .into_iter()
    .filter(|(provider, _)| match provider {
        ProviderId::Anthropic => has_anthropic,
        ProviderId::Codex => has_codex,
    })
    .map(|(provider, id)| {
        json!({
            "id": id,
            "object": "model",
            "created": created,
            "owned_by": provider.to_string()
        })
    })
    .collect::<Vec<_>>();

    Json(json!({"object": "list", "data": models})).into_response()
}

async fn chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let body = match parse_request(&state, &headers, &body) {
        Ok(body) => body,
        Err(error) => return error.into_response(),
    };
    if !non_empty_array(body.get("messages")) {
        return AppError::simple(
            StatusCode::BAD_REQUEST,
            "messages is required and must be a non-empty array",
        )
        .into_response();
    }
    route_provider_request(&state, &headers, &body, RequestRoute::Chat).await
}

async fn responses(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let body = match parse_request(&state, &headers, &body) {
        Ok(body) => body,
        Err(error) => return error.into_response(),
    };
    if body.get("input").is_none() && body.get("messages").is_none() {
        return AppError::simple(StatusCode::BAD_REQUEST, "input is required").into_response();
    }
    route_provider_request(&state, &headers, &body, RequestRoute::Responses).await
}

async fn messages(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let body = match parse_request(&state, &headers, &body) {
        Ok(body) => body,
        Err(error) => return error.into_response(),
    };
    if !non_empty_array(body.get("messages")) {
        return AppError::simple(
            StatusCode::BAD_REQUEST,
            "messages is required and must be a non-empty array",
        )
        .into_response();
    }
    route_provider_request(&state, &headers, &body, RequestRoute::Messages).await
}

async fn count_tokens(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let body = match parse_request(&state, &headers, &body) {
        Ok(body) => body,
        Err(error) => return error.into_response(),
    };
    let model = resolve_model(body.get("model").and_then(Value::as_str));
    let provider = state.registry.for_model(&model);
    if provider.id == ProviderId::Codex {
        return AppError::provider(
            StatusCode::NOT_IMPLEMENTED,
            "count_tokens is not supported for the codex provider",
            "unsupported_endpoint_for_provider",
            provider.id,
        )
        .into_response();
    }
    let account = match next_provider_account(&state, provider.id).await {
        Ok(account) => account,
        Err(error) => return error.into_response(),
    };
    let body = body_with_model(&body, &model);
    match state
        .upstream
        .anthropic_count_tokens(UpstreamRequest {
            body,
            request_headers: headers_to_map(&headers),
            account: account.clone(),
            config: (*state.config).clone(),
        })
        .await
    {
        Ok(response) => {
            if response.status.is_success() {
                record_provider_success(&state, provider.id, &account, None).await;
            } else {
                record_provider_failure(&state, provider.id, &account, response.status, None).await;
            }
            (response.status, Json(response.body)).into_response()
        }
        Err(error) => {
            record_provider_failure(
                &state,
                provider.id,
                &account,
                StatusCode::BAD_GATEWAY,
                Some(&error.to_string()),
            )
            .await;
            upstream_error_response(provider.id, &error)
        }
    }
}

fn parse_request(state: &AppState, headers: &HeaderMap, body: &[u8]) -> Result<Value, AppError> {
    require_api_key(state, headers, true)?;
    enforce_body_limit(state, headers)?;
    serde_json::from_slice(body)
        .map_err(|_| AppError::simple(StatusCode::BAD_REQUEST, "invalid JSON body"))
}

async fn route_provider_request(
    state: &AppState,
    headers: &HeaderMap,
    body: &Value,
    route: RequestRoute,
) -> Response {
    let model = resolve_model(body.get("model").and_then(Value::as_str));
    let provider = state.registry.for_model(&model);
    let client_wants_stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let attempts = provider_account_count(state, provider.id).await.max(1);
    let mut last_response = None;

    for _ in 0..attempts {
        let account = match next_provider_account(state, provider.id).await {
            Ok(account) => account,
            Err(error) if error.error_type == Some("token_refresh_failed") => {
                last_response = Some(error.into_response());
                continue;
            }
            Err(error) => return last_response.unwrap_or_else(|| error.into_response()),
        };
        let response = if provider.id == ProviderId::Codex {
            route_codex_request(
                state,
                headers,
                body,
                route,
                &model,
                &account,
                client_wants_stream,
            )
            .await
        } else {
            route_anthropic_request(
                state,
                headers,
                body,
                route,
                &model,
                &account,
                client_wants_stream,
            )
            .await
        };
        if !should_retry_upstream_status(response.status()) {
            return response;
        }
        last_response = Some(response);
    }

    last_response.unwrap_or_else(|| {
        AppError::provider(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("no available {} account", provider.id),
            "no_account_for_provider",
            provider.id,
        )
        .into_response()
    })
}

async fn provider_account_count(state: &AppState, provider: ProviderId) -> usize {
    match provider {
        ProviderId::Anthropic => state
            .account_managers
            .anthropic
            .lock()
            .await
            .account_count(),
        ProviderId::Codex => state.account_managers.codex.lock().await.account_count(),
    }
}

fn should_retry_upstream_status(status: StatusCode) -> bool {
    matches!(status.as_u16(), 401 | 403 | 429 | 500..=599)
}

async fn route_codex_request(
    state: &AppState,
    headers: &HeaderMap,
    body: &Value,
    route: RequestRoute,
    model: &str,
    account: &AvailableAccount,
    client_wants_stream: bool,
) -> Response {
    let body = codex_request_body(body, model, route);
    if client_wants_stream {
        return match state
            .upstream
            .codex_responses_stream(UpstreamRequest {
                body,
                request_headers: headers_to_map(headers),
                account: account.clone(),
                config: (*state.config).clone(),
            })
            .await
        {
            Ok(response) => {
                let accounting =
                    stream_accounting(state, ProviderId::Codex, account, response.status).await;
                sse_upstream_response(response, ProviderId::Codex, route, model, accounting)
            }
            Err(error) => {
                upstream_failure_response(state, ProviderId::Codex, account, &error).await
            }
        };
    }
    match state
        .upstream
        .codex_responses(UpstreamRequest {
            body,
            request_headers: headers_to_map(headers),
            account: account.clone(),
            config: (*state.config).clone(),
        })
        .await
    {
        Ok(response) => {
            record_json_result(state, ProviderId::Codex, account, &response).await;
            json_upstream_response(response, ProviderId::Codex, route, model)
        }
        Err(error) => upstream_failure_response(state, ProviderId::Codex, account, &error).await,
    }
}

async fn route_anthropic_request(
    state: &AppState,
    headers: &HeaderMap,
    body: &Value,
    route: RequestRoute,
    model: &str,
    account: &AvailableAccount,
    client_wants_stream: bool,
) -> Response {
    let body = anthropic_request_body(body, model, route);
    if client_wants_stream {
        return match state
            .upstream
            .anthropic_messages_stream(UpstreamRequest {
                body,
                request_headers: headers_to_map(headers),
                account: account.clone(),
                config: (*state.config).clone(),
            })
            .await
        {
            Ok(response) => {
                let accounting =
                    stream_accounting(state, ProviderId::Anthropic, account, response.status).await;
                sse_upstream_response(response, ProviderId::Anthropic, route, model, accounting)
            }
            Err(error) => {
                upstream_failure_response(state, ProviderId::Anthropic, account, &error).await
            }
        };
    }
    match state
        .upstream
        .anthropic_messages(UpstreamRequest {
            body,
            request_headers: headers_to_map(headers),
            account: account.clone(),
            config: (*state.config).clone(),
        })
        .await
    {
        Ok(response) => {
            record_json_result(state, ProviderId::Anthropic, account, &response).await;
            json_upstream_response(response, ProviderId::Anthropic, route, model)
        }
        Err(error) => {
            upstream_failure_response(state, ProviderId::Anthropic, account, &error).await
        }
    }
}

async fn stream_accounting(
    state: &AppState,
    provider: ProviderId,
    account: &AvailableAccount,
    status: StatusCode,
) -> Option<StreamAccounting> {
    if status.is_success() {
        Some(StreamAccounting {
            state: state.clone(),
            provider,
            account: account.clone(),
        })
    } else {
        record_provider_failure(state, provider, account, status, None).await;
        None
    }
}

async fn record_json_result(
    state: &AppState,
    provider: ProviderId,
    account: &AvailableAccount,
    response: &UpstreamJsonResponse,
) {
    if response.status.is_success() {
        record_provider_success(
            state,
            provider,
            account,
            usage_from_response(&response.body),
        )
        .await;
    } else {
        record_provider_failure(state, provider, account, response.status, None).await;
    }
}

async fn upstream_failure_response(
    state: &AppState,
    provider: ProviderId,
    account: &AvailableAccount,
    error: &anyhow::Error,
) -> Response {
    record_provider_failure(
        state,
        provider,
        account,
        StatusCode::BAD_GATEWAY,
        Some(&error.to_string()),
    )
    .await;
    upstream_error_response(provider, error)
}

fn require_api_key(
    state: &AppState,
    headers: &HeaderMap,
    apply_rate_limit: bool,
) -> Result<(), AppError> {
    let Some(api_key) = extract_api_key(headers) else {
        return Err(AppError::simple(
            StatusCode::UNAUTHORIZED,
            "missing API key",
        ));
    };
    if !state.config.api_keys.contains(&api_key) {
        return Err(AppError::simple(StatusCode::FORBIDDEN, "invalid API key"));
    }
    if apply_rate_limit {
        enforce_rate_limit(state, headers)?;
    }
    Ok(())
}

fn enforce_rate_limit(state: &AppState, headers: &HeaderMap) -> Result<(), AppError> {
    let key = rate_limit_key(headers);
    let now = Instant::now();
    let mut buckets = state.rate_limit_buckets.lock().map_err(|_| {
        AppError::simple(
            StatusCode::INTERNAL_SERVER_ERROR,
            "rate-limit bucket lock is poisoned",
        )
    })?;
    let bucket = buckets.entry(key).or_insert(RateLimitBucket {
        count: 0,
        reset_at: now + RATE_LIMIT_WINDOW,
    });
    if now > bucket.reset_at {
        bucket.count = 1;
        bucket.reset_at = now + RATE_LIMIT_WINDOW;
        return Ok(());
    }
    bucket.count += 1;
    if bucket.count > RATE_LIMIT_MAX {
        return Err(AppError::simple(
            StatusCode::TOO_MANY_REQUESTS,
            "too many requests",
        ));
    }
    Ok(())
}

fn rate_limit_key(headers: &HeaderMap) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            headers
                .get("x-real-ip")
                .and_then(|value| value.to_str().ok())
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
        .unwrap_or("unknown")
        .to_string()
}

fn extract_api_key(headers: &HeaderMap) -> Option<String> {
    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            headers
                .get("x-api-key")
                .and_then(|value| value.to_str().ok())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        })
}

fn enforce_body_limit(state: &AppState, headers: &HeaderMap) -> Result<(), AppError> {
    let BodyLimit::Limited(limit) = state.body_limit else {
        return match state.body_limit {
            BodyLimit::Invalid => Err(AppError::simple(
                StatusCode::INTERNAL_SERVER_ERROR,
                "invalid body-limit",
            )),
            BodyLimit::Unlimited => Ok(()),
            BodyLimit::Limited(_) => unreachable!(),
        };
    };

    let Some(content_length) = headers.get(CONTENT_LENGTH) else {
        return Err(AppError::simple(
            StatusCode::LENGTH_REQUIRED,
            "missing content-length",
        ));
    };
    let declared_length = content_length
        .to_str()
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| AppError::simple(StatusCode::BAD_REQUEST, "invalid content-length"))?;
    if declared_length > limit {
        return Err(AppError::simple(
            StatusCode::PAYLOAD_TOO_LARGE,
            "request body too large",
        ));
    }
    Ok(())
}

async fn next_provider_account(
    state: &AppState,
    provider: ProviderId,
) -> Result<AvailableAccount, AppError> {
    let mut manager = match provider {
        ProviderId::Anthropic => state.account_managers.anthropic.lock().await,
        ProviderId::Codex => state.account_managers.codex.lock().await,
    };
    let result = manager.next_account_result();
    let Some(account) = result.account else {
        return Err(AppError::provider(
            StatusCode::SERVICE_UNAVAILABLE,
            no_account_message(
                provider,
                result.failure_kind.as_deref(),
                result.retry_after_seconds,
            ),
            "no_account_for_provider",
            provider,
        ));
    };
    let email = account.token.email.clone();
    manager.record_attempt(&email);
    match manager.refresh_if_due(&email).await {
        Ok(true) => {}
        Ok(false) => {
            return Err(AppError::provider(
                StatusCode::BAD_GATEWAY,
                format!("failed to refresh {provider} account; re-run login for {provider}"),
                "token_refresh_failed",
                provider,
            ));
        }
        Err(error) => {
            manager.record_failure(&email, "auth", Some(&error.to_string()));
            return Err(AppError::provider(
                StatusCode::BAD_GATEWAY,
                format!("failed to refresh {provider} account: {error}"),
                "token_refresh_failed",
                provider,
            ));
        }
    }
    Ok(manager.account(&email).unwrap_or(account))
}

async fn record_provider_success(
    state: &AppState,
    provider: ProviderId,
    account: &AvailableAccount,
    usage: Option<UsageData>,
) {
    let mut manager = match provider {
        ProviderId::Anthropic => state.account_managers.anthropic.lock().await,
        ProviderId::Codex => state.account_managers.codex.lock().await,
    };
    manager.record_success(account.token.email.as_str(), usage.as_ref());
}

async fn record_provider_failure(
    state: &AppState,
    provider: ProviderId,
    account: &AvailableAccount,
    status: StatusCode,
    detail: Option<&str>,
) {
    let mut manager = match provider {
        ProviderId::Anthropic => state.account_managers.anthropic.lock().await,
        ProviderId::Codex => state.account_managers.codex.lock().await,
    };
    manager.record_failure(
        account.token.email.as_str(),
        classify_status(status),
        detail,
    );
}

fn no_account_message(
    provider: ProviderId,
    failure_kind: Option<&str>,
    retry_after_seconds: Option<f64>,
) -> String {
    let mut message = format!("no available {provider} account; run login for {provider}");
    if let Some(failure_kind) = failure_kind {
        write!(message, "; last failure: {failure_kind}").expect("write to String cannot fail");
    }
    if let Some(retry_after_seconds) = retry_after_seconds {
        write!(
            message,
            "; retry after {} seconds",
            retry_after_seconds.ceil()
        )
        .expect("write to String cannot fail");
    }
    message
}

fn classify_status(status: StatusCode) -> &'static str {
    match status.as_u16() {
        401 => "auth",
        403 => "forbidden",
        429 => "rate_limit",
        500..=599 => "server",
        _ => "network",
    }
}

fn usage_from_response(body: &Value) -> Option<UsageData> {
    let usage = body.get("usage")?;
    let input_tokens = usage
        .get("input_tokens")
        .or_else(|| usage.get("prompt_tokens"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let output_tokens = usage
        .get("output_tokens")
        .or_else(|| usage.get("completion_tokens"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    Some(UsageData {
        input_tokens,
        output_tokens,
        cache_creation_input_tokens: usage
            .get("cache_creation_input_tokens")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        cache_read_input_tokens: usage
            .get("cache_read_input_tokens")
            .or_else(|| {
                usage
                    .get("input_tokens_details")
                    .and_then(|details| details.get("cached_tokens"))
            })
            .and_then(Value::as_i64)
            .unwrap_or(0),
        reasoning_output_tokens: usage
            .get("output_tokens_details")
            .and_then(|details| details.get("reasoning_tokens"))
            .and_then(Value::as_i64)
            .unwrap_or(0),
    })
}

fn upstream_error_response(provider: ProviderId, error: &anyhow::Error) -> Response {
    AppError::provider(
        StatusCode::BAD_GATEWAY,
        format!("upstream request failed: {error}"),
        "network_error",
        provider,
    )
    .into_response()
}

fn json_upstream_response(
    response: UpstreamJsonResponse,
    provider: ProviderId,
    route: RequestRoute,
    model: &str,
) -> Response {
    if !response.status.is_success() {
        return (response.status, Json(response.body)).into_response();
    }
    let body = match (provider, route) {
        (ProviderId::Anthropic, RequestRoute::Chat) => anthropic_to_openai(&response.body, model),
        (ProviderId::Anthropic, RequestRoute::Responses) => {
            anthropic_to_responses(&response.body, model)
        }
        (ProviderId::Anthropic, RequestRoute::Messages)
        | (ProviderId::Codex, RequestRoute::Responses) => response.body,
        (ProviderId::Codex, RequestRoute::Chat) => {
            responses_to_chat_completion(&response.body, model)
        }
        (ProviderId::Codex, RequestRoute::Messages) => {
            responses_to_anthropic_message(&response.body, model)
        }
    };
    (response.status, Json(body)).into_response()
}

fn sse_upstream_response(
    response: UpstreamSseResponse,
    provider: ProviderId,
    route: RequestRoute,
    model: &str,
    accounting: Option<StreamAccounting>,
) -> Response {
    let body = if response.status.is_success() {
        transformed_sse_stream(
            response.body,
            provider,
            route,
            model.to_string(),
            accounting,
        )
    } else {
        response.body
    };
    Response::builder()
        .status(response.status)
        .header(CONTENT_TYPE, "text/event-stream; charset=utf-8")
        .body(Body::from_stream(body))
        .unwrap_or_else(|error| {
            AppError::provider(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to build stream response: {error}"),
                "internal_error",
                provider,
            )
            .into_response()
        })
}

fn transformed_sse_stream(
    mut input: UpstreamSseStream,
    provider: ProviderId,
    route: RequestRoute,
    model: String,
    accounting: Option<StreamAccounting>,
) -> UpstreamSseStream {
    Box::pin(try_stream! {
        let mut buffer = Vec::new();
        let mut chat_state = ChatStreamState::new(model.clone());
        let mut responses_state = ResponsesStreamState::new(model.clone());
        let mut anthropic_state = AnthropicStreamState::new(model.clone());
        let mut usage = UsageData::default();
        let mut completed = false;

        while let Some(chunk) = input.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(error) => {
                    record_stream_failure(accounting.as_ref(), &error.to_string()).await;
                    Err(error)?;
                    unreachable!();
                }
            };
            buffer.extend_from_slice(&chunk);
            let events = match drain_complete_sse_events(&mut buffer) {
                Ok(events) => events,
                Err(error) => {
                    record_stream_failure(accounting.as_ref(), &error.to_string()).await;
                    Err(error)?;
                    unreachable!();
                }
            };
            for (event, raw) in events {
                update_stream_usage(provider, &event, &raw, &mut usage, &mut completed);
                for chunk in transform_sse_event(
                    provider,
                    route,
                    &model,
                    &mut chat_state,
                    &mut responses_state,
                    &mut anthropic_state,
                    &event,
                    &raw,
                ) {
                    yield Bytes::from(chunk);
                }
            }
        }
        let events = match finish_sse_events(&mut buffer) {
            Ok(events) => events,
            Err(error) => {
                record_stream_failure(accounting.as_ref(), &error.to_string()).await;
                Err(error)?;
                unreachable!();
            }
        };
        for (event, raw) in events {
            update_stream_usage(provider, &event, &raw, &mut usage, &mut completed);
            for chunk in transform_sse_event(
                provider,
                route,
                &model,
                &mut chat_state,
                &mut responses_state,
                &mut anthropic_state,
                &event,
                &raw,
            ) {
                yield Bytes::from(chunk);
            }
        }
        if completed {
            record_stream_success(accounting.as_ref(), &usage).await;
        } else {
            record_stream_failure(accounting.as_ref(), "stream terminated before completion").await;
        }
    })
}

async fn record_stream_success(accounting: Option<&StreamAccounting>, usage: &UsageData) {
    if let Some(accounting) = accounting {
        record_provider_success(
            &accounting.state,
            accounting.provider,
            &accounting.account,
            Some(usage.clone()),
        )
        .await;
    }
}

async fn record_stream_failure(accounting: Option<&StreamAccounting>, detail: &str) {
    if let Some(accounting) = accounting {
        record_provider_failure(
            &accounting.state,
            accounting.provider,
            &accounting.account,
            StatusCode::BAD_GATEWAY,
            Some(detail),
        )
        .await;
    }
}

fn update_stream_usage(
    provider: ProviderId,
    event: &str,
    raw: &str,
    usage: &mut UsageData,
    completed: &mut bool,
) {
    if raw == "[DONE]" {
        *completed = true;
        return;
    }
    let Ok(data) = serde_json::from_str::<Value>(raw) else {
        return;
    };
    match provider {
        ProviderId::Anthropic => update_anthropic_stream_usage(event, &data, usage, completed),
        ProviderId::Codex => update_codex_stream_usage(event, &data, usage, completed),
    }
}

fn update_anthropic_stream_usage(
    event: &str,
    data: &Value,
    usage: &mut UsageData,
    completed: &mut bool,
) {
    match event {
        "message_start" => {
            if let Some(payload) = data.get("message").and_then(|message| message.get("usage")) {
                usage.input_tokens = int_field_or(payload, "input_tokens", usage.input_tokens);
                usage.cache_creation_input_tokens = int_field_or(
                    payload,
                    "cache_creation_input_tokens",
                    usage.cache_creation_input_tokens,
                );
                usage.cache_read_input_tokens = int_field_or(
                    payload,
                    "cache_read_input_tokens",
                    usage.cache_read_input_tokens,
                );
            }
        }
        "message_delta" => {
            if let Some(payload) = data.get("usage") {
                usage.output_tokens = int_field_or(payload, "output_tokens", usage.output_tokens);
            }
        }
        "message_stop" => *completed = true,
        _ => {}
    }
}

fn update_codex_stream_usage(
    event: &str,
    data: &Value,
    usage: &mut UsageData,
    completed: &mut bool,
) {
    if matches!(event, "response.completed" | "response.incomplete") {
        *completed = true;
        let response = data.get("response").unwrap_or(data);
        if let Some(next_usage) = usage_from_response(response) {
            *usage = next_usage;
        }
    }
}

fn int_field_or(value: &Value, key: &str, default: i64) -> i64 {
    value.get(key).and_then(Value::as_i64).unwrap_or(default)
}

#[allow(clippy::too_many_arguments)]
fn transform_sse_event(
    provider: ProviderId,
    route: RequestRoute,
    model: &str,
    chat_state: &mut ChatStreamState,
    responses_state: &mut ResponsesStreamState,
    anthropic_state: &mut AnthropicStreamState,
    event: &str,
    raw: &str,
) -> Vec<String> {
    if raw == "[DONE]" {
        return match (provider, route) {
            (ProviderId::Anthropic, RequestRoute::Messages)
            | (ProviderId::Codex, RequestRoute::Responses) => vec!["data: [DONE]\n\n".to_string()],
            _ => Vec::new(),
        };
    }

    let parsed = serde_json::from_str::<Value>(raw);
    match (provider, route) {
        (ProviderId::Anthropic, RequestRoute::Messages)
        | (ProviderId::Codex, RequestRoute::Responses) => parsed.map_or_else(
            |_| {
                vec![sse(
                    &Value::String(raw.to_string()),
                    passthrough_event(event),
                )]
            },
            |data| vec![sse(&data, passthrough_event(event))],
        ),
        (ProviderId::Anthropic, RequestRoute::Chat) => parsed.map_or_else(
            |_| Vec::new(),
            |data| anthropic_sse_to_chat(event, &data, chat_state),
        ),
        (ProviderId::Anthropic, RequestRoute::Responses) => parsed.map_or_else(
            |_| Vec::new(),
            |data| anthropic_sse_to_responses(event, &data, responses_state, model),
        ),
        (ProviderId::Codex, RequestRoute::Chat) => parsed.map_or_else(
            |_| Vec::new(),
            |data| responses_sse_to_chat(event, &data, chat_state),
        ),
        (ProviderId::Codex, RequestRoute::Messages) => parsed.map_or_else(
            |_| Vec::new(),
            |data| responses_sse_to_anthropic(event, &data, anthropic_state),
        ),
    }
}

fn passthrough_event(event: &str) -> Option<&str> {
    (event != "message").then_some(event)
}

fn headers_to_map(headers: &HeaderMap) -> BTreeMap<String, String> {
    headers
        .iter()
        .filter_map(|(key, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (key.as_str().to_ascii_lowercase(), value.to_string()))
        })
        .collect()
}

fn body_with_model(body: &Value, model: &str) -> Value {
    let mut next_body = body.clone();
    if let Some(object) = next_body.as_object_mut() {
        object.insert("model".to_string(), Value::String(model.to_string()));
    }
    next_body
}

fn anthropic_request_body(body: &Value, model: &str, route: RequestRoute) -> Value {
    let translated = match route {
        RequestRoute::Chat => openai_to_anthropic(body),
        RequestRoute::Responses => responses_to_anthropic(body),
        RequestRoute::Messages => body.clone(),
    };
    body_with_model(&translated, model)
}

fn codex_request_body(body: &Value, model: &str, route: RequestRoute) -> Value {
    let translated = match route {
        RequestRoute::Chat => chat_to_responses_request(body),
        RequestRoute::Responses => body.clone(),
        RequestRoute::Messages => anthropic_to_responses_request(body),
    };
    let mut normalized = normalize_codex_responses_body(&body_with_model(&translated, model));
    if let Some(object) = normalized.as_object_mut() {
        object.insert("stream".to_string(), Value::Bool(true));
        object.remove("max_output_tokens");
        object.remove("parallel_tool_calls");
    }
    normalized
}

/// Build a POST request with a JSON body and provider headers.
///
/// `.json()` already sets `Content-Type: application/json`, so any `content-type` entry in
/// `headers` is skipped to avoid sending a duplicate header. The Codex backend rejects a
/// duplicate `Content-Type` with "Unsupported content type".
fn build_upstream_request(
    client: &reqwest::Client,
    url: &str,
    headers: BTreeMap<String, String>,
    body: &Value,
    timeout_ms: u64,
) -> reqwest::RequestBuilder {
    let mut request = client
        .post(url)
        .timeout(std::time::Duration::from_millis(timeout_ms))
        .json(body);
    for (key, value) in headers {
        if key.eq_ignore_ascii_case("content-type") {
            continue;
        }
        request = request.header(key, value);
    }
    request
}

async fn send_json(
    client: reqwest::Client,
    url: String,
    headers: BTreeMap<String, String>,
    body: Value,
    timeout_ms: u64,
) -> anyhow::Result<UpstreamJsonResponse> {
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("claude-sonnet-4-6")
        .to_string();
    let response = build_upstream_request(&client, &url, headers, &body, timeout_ms)
        .send()
        .await?;
    let mut status = StatusCode::from_u16(response.status().as_u16())?;
    let headers = response.headers().clone();
    let bytes = response.bytes().await?;
    let body = decode_upstream_body(&headers, &bytes, &model);
    if status.is_success() && is_decoded_upstream_error(&body) {
        status = StatusCode::BAD_GATEWAY;
    }
    Ok(UpstreamJsonResponse { status, body })
}

async fn send_stream(
    client: reqwest::Client,
    url: String,
    headers: BTreeMap<String, String>,
    body: Value,
    timeout_ms: u64,
) -> anyhow::Result<UpstreamSseResponse> {
    let response = build_upstream_request(&client, &url, headers, &body, timeout_ms)
        .send()
        .await?;
    let status = StatusCode::from_u16(response.status().as_u16())?;
    Ok(UpstreamSseResponse {
        status,
        body: Box::pin(response.bytes_stream().map_err(anyhow::Error::from)),
    })
}

fn decode_upstream_body(headers: &HeaderMap, bytes: &[u8], model: &str) -> Value {
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if content_type
        .to_ascii_lowercase()
        .starts_with("text/event-stream")
    {
        return responses_sse_to_payload(&[bytes], model).unwrap_or_else(|error| {
            json!({
                "error": {
                    "message": format!("failed to parse upstream event stream: {error}")
                }
            })
        });
    }

    serde_json::from_slice(bytes).unwrap_or_else(|_| {
        json!({
            "error": {
                "message": String::from_utf8_lossy(bytes)
            }
        })
    })
}

fn is_decoded_upstream_error(body: &Value) -> bool {
    body.get("error")
        .and_then(|error| error.get("type"))
        .and_then(Value::as_str)
        == Some("upstream_error")
}

fn non_empty_array(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_array)
        .is_some_and(|value| !value.is_empty())
}

fn parse_body_limit(value: &str) -> BodyLimit {
    let raw = value.trim().to_ascii_lowercase();
    if raw.is_empty() {
        return BodyLimit::Unlimited;
    }
    for (suffix, multiplier) in [
        ("gb", 1024_u64 * 1024 * 1024),
        ("mb", 1024_u64 * 1024),
        ("kb", 1024_u64),
        ("b", 1_u64),
    ] {
        if let Some(number) = raw.strip_suffix(suffix) {
            return parse_limit_number(number.trim(), multiplier);
        }
    }
    parse_limit_number(&raw, 1)
}

fn parse_limit_number(number: &str, multiplier: u64) -> BodyLimit {
    let Ok(value) = number.parse::<u64>() else {
        return BodyLimit::Invalid;
    };
    value
        .checked_mul(multiplier)
        .map_or(BodyLimit::Invalid, BodyLimit::Limited)
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};

    use anyhow::{Result, bail};
    use axum::http::{HeaderMap, StatusCode};
    use http_body_util::BodyExt;

    use super::{
        AccountManagers, AppState, BodyLimit, RateLimitBucket, RequestRoute, UpstreamClient,
        UpstreamFuture, UpstreamJsonResponse, UpstreamRequest, UpstreamSseFuture,
        build_upstream_request, decode_upstream_body, is_decoded_upstream_error,
        route_provider_request,
    };
    use crate::accounts::{AccountManager, RefreshPolicy};
    use crate::config::{CloakingConfig, Config, DebugMode, TimeoutConfig};
    use crate::providers::build_registry;
    use crate::tokens::save_token;
    use crate::types::{ProviderId, TokenData};
    use serde_json::{Value, json};
    use std::collections::BTreeMap;

    #[test]
    fn upstream_request_sends_single_content_type() {
        let headers = BTreeMap::from([
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Accept".to_string(), "text/event-stream".to_string()),
            ("Authorization".to_string(), "Bearer token".to_string()),
        ]);
        let body = json!({"model": "gpt-5.5", "stream": true});
        let request = build_upstream_request(
            &reqwest::Client::new(),
            "https://chatgpt.com/backend-api/codex/responses",
            headers,
            &body,
            30_000,
        )
        .build()
        .expect("request builds");
        let content_types: Vec<_> = request
            .headers()
            .get_all(reqwest::header::CONTENT_TYPE)
            .iter()
            .collect();
        assert_eq!(content_types.len(), 1, "exactly one Content-Type header");
        assert_eq!(content_types[0], "application/json");
        assert_eq!(
            request
                .headers()
                .get(reqwest::header::ACCEPT)
                .and_then(|value| value.to_str().ok()),
            Some("text/event-stream"),
        );
    }

    #[derive(Default)]
    struct CapturingUpstream {
        calls: Mutex<Vec<String>>,
    }

    impl UpstreamClient for CapturingUpstream {
        fn anthropic_messages(&self, request: UpstreamRequest) -> UpstreamFuture {
            self.calls
                .lock()
                .expect("calls lock")
                .push(request.account.token.email);
            Box::pin(async {
                Ok(UpstreamJsonResponse {
                    status: StatusCode::OK,
                    body: json!({
                        "id": "msg_1",
                        "type": "message",
                        "role": "assistant",
                        "model": "claude-sonnet-4-6",
                        "content": [{"type": "text", "text": "pong"}],
                        "usage": {"input_tokens": 1, "output_tokens": 1}
                    }),
                })
            })
        }

        fn anthropic_messages_stream(&self, _request: UpstreamRequest) -> UpstreamSseFuture {
            unreachable!("stream not used in refresh fallback test")
        }

        fn anthropic_count_tokens(&self, _request: UpstreamRequest) -> UpstreamFuture {
            unreachable!("count_tokens not used in refresh fallback test")
        }

        fn codex_responses(&self, _request: UpstreamRequest) -> UpstreamFuture {
            unreachable!("codex not used in refresh fallback test")
        }

        fn codex_responses_stream(&self, _request: UpstreamRequest) -> UpstreamSseFuture {
            unreachable!("codex stream not used in refresh fallback test")
        }
    }

    fn token(email: &str, access_token: &str, expires_at: &str) -> TokenData {
        TokenData {
            access_token: access_token.to_string(),
            refresh_token: format!("{access_token}-refresh"),
            email: email.to_string(),
            expires_at: expires_at.to_string(),
            account_uuid: email.to_string(),
            provider: ProviderId::Anthropic,
            id_token: None,
            last_refresh_at: None,
            plan_type: None,
        }
    }

    #[test]
    fn decode_upstream_body_drains_event_stream_payloads() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("text/event-stream; charset=utf-8"),
        );

        let body = decode_upstream_body(
            &headers,
            concat!(
                "event: response.output_text.delta\n",
                "data: {\"delta\":\"ok\"}\n\n",
                "event: response.completed\n",
                "data: {\"response\":{\"id\":\"resp_1\",\"object\":\"response\",\"status\":\"completed\",\"model\":\"gpt-5.4\"}}\n\n"
            )
            .as_bytes(),
            "gpt-5.4",
        );

        assert_eq!(body["id"], "resp_1");
        assert_eq!(body["output_text"], "ok");
    }

    #[test]
    fn decode_upstream_body_flags_event_stream_failures() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("text/event-stream; charset=utf-8"),
        );

        let body = decode_upstream_body(
            &headers,
            concat!(
                "event: response.failed\n",
                "data: {\"error\":{\"message\":\"model overloaded\"}}\n\n"
            )
            .as_bytes(),
            "gpt-5.4",
        );

        assert!(is_decoded_upstream_error(&body));
        assert_eq!(body["error"]["message"], "model overloaded");
    }

    #[tokio::test]
    async fn route_tries_next_account_when_first_refresh_fails() {
        let tmp = tempfile::tempdir().expect("tempdir");
        save_token(
            tmp.path(),
            &token(
                "alice@example.com",
                "anthropic-access-alice",
                "2000-01-01T00:00:00Z",
            ),
        )
        .expect("save alice");
        save_token(
            tmp.path(),
            &token(
                "bob@example.com",
                "anthropic-access-bob",
                "2030-01-01T00:00:00Z",
            ),
        )
        .expect("save bob");

        let mut anthropic = AccountManager::new(
            tmp.path().to_path_buf(),
            ProviderId::Anthropic,
            |_refresh_token| {
                Box::pin(async {
                    bail!("refresh failed");
                }) as Pin<Box<dyn Future<Output = Result<TokenData>> + Send>>
            },
            RefreshPolicy::default(),
        );
        anthropic.load().expect("load anthropic accounts");
        let codex = AccountManager::new(
            tmp.path().to_path_buf(),
            ProviderId::Codex,
            |_refresh_token| {
                Box::pin(async {
                    bail!("unused refresh");
                }) as Pin<Box<dyn Future<Output = Result<TokenData>> + Send>>
            },
            RefreshPolicy::default(),
        );
        let upstream = Arc::new(CapturingUpstream::default());
        let config = Config {
            host: String::new(),
            port: 8317,
            auth_dir: tmp.path().to_path_buf(),
            api_keys: std::collections::HashSet::new(),
            body_limit: String::new(),
            cloaking: CloakingConfig {
                cli_version: "2.1.88".to_string(),
                entrypoint: "cli".to_string(),
                codex: std::collections::BTreeMap::new(),
            },
            timeouts: TimeoutConfig {
                messages_ms: 120_000,
                stream_messages_ms: 600_000,
                count_tokens_ms: 30_000,
            },
            stats_enabled: true,
            debug: DebugMode::Off,
        };
        let state = AppState {
            config: Arc::new(config),
            registry: Arc::new(build_registry(tmp.path())),
            body_limit: BodyLimit::Unlimited,
            upstream: upstream.clone(),
            account_managers: Arc::new(AccountManagers {
                anthropic: tokio::sync::Mutex::new(anthropic),
                codex: tokio::sync::Mutex::new(codex),
            }),
            rate_limit_buckets: Arc::new(Mutex::new(std::collections::BTreeMap::<
                String,
                RateLimitBucket,
            >::new())),
        };

        let response = route_provider_request(
            &state,
            &HeaderMap::new(),
            &json!({
                "model": "sonnet",
                "messages": [{"role": "user", "content": "reply exactly: pong"}]
            }),
            RequestRoute::Messages,
        )
        .await;
        let status = response.status();
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        let body = serde_json::from_slice::<Value>(&body).expect("json body");

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["content"][0]["text"], "pong");
        assert_eq!(
            *upstream.calls.lock().expect("calls lock"),
            ["bob@example.com"]
        );
    }
}
