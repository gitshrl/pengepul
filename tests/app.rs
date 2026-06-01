use std::collections::{BTreeMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use axum::body::{Body, Bytes};
use http_body_util::BodyExt;
use pengepul::app::{
    UpstreamClient, UpstreamJsonResponse, UpstreamRequest, UpstreamSseResponse, create_app,
    create_app_with_upstream,
};
use pengepul::config::{CloakingConfig, Config, DebugMode, TimeoutConfig};
use pengepul::tokens::save_token;
use pengepul::types::{ProviderId, TokenData};
use serde_json::{Value, json};
use tower::ServiceExt;

#[derive(Default)]
struct FakeUpstream {
    calls: Mutex<Vec<UpstreamRequest>>,
}

impl FakeUpstream {
    fn calls(&self) -> Vec<UpstreamRequest> {
        self.calls.lock().expect("calls lock").clone()
    }
}

#[derive(Default)]
struct RetryUpstream {
    calls: Mutex<Vec<UpstreamRequest>>,
}

impl RetryUpstream {
    fn calls(&self) -> Vec<UpstreamRequest> {
        self.calls.lock().expect("calls lock").clone()
    }
}

impl UpstreamClient for RetryUpstream {
    fn anthropic_messages(
        &self,
        request: UpstreamRequest,
    ) -> Pin<Box<dyn Future<Output = Result<UpstreamJsonResponse>> + Send>> {
        let status = if request.account.token.access_token.contains("alice") {
            axum::http::StatusCode::TOO_MANY_REQUESTS
        } else {
            axum::http::StatusCode::OK
        };
        self.calls.lock().expect("calls lock").push(request);
        Box::pin(async move {
            Ok(UpstreamJsonResponse {
                status,
                body: if status.is_success() {
                    json!({
                        "id": "msg_1",
                        "type": "message",
                        "role": "assistant",
                        "model": "claude-sonnet-4-6",
                        "content": [{"type": "text", "text": "pong"}],
                        "usage": {"input_tokens": 1, "output_tokens": 1}
                    })
                } else {
                    json!({"error": {"message": "rate limited"}})
                },
            })
        })
    }

    fn anthropic_messages_stream(
        &self,
        _request: UpstreamRequest,
    ) -> Pin<Box<dyn Future<Output = Result<UpstreamSseResponse>> + Send>> {
        unreachable!("stream not used in retry test")
    }

    fn anthropic_count_tokens(
        &self,
        _request: UpstreamRequest,
    ) -> Pin<Box<dyn Future<Output = Result<UpstreamJsonResponse>> + Send>> {
        unreachable!("count_tokens not used in retry test")
    }

    fn codex_responses(
        &self,
        _request: UpstreamRequest,
    ) -> Pin<Box<dyn Future<Output = Result<UpstreamJsonResponse>> + Send>> {
        unreachable!("codex not used in retry test")
    }

    fn codex_responses_stream(
        &self,
        _request: UpstreamRequest,
    ) -> Pin<Box<dyn Future<Output = Result<UpstreamSseResponse>> + Send>> {
        unreachable!("codex stream not used in retry test")
    }

    fn opencode_go_chat(
        &self,
        _request: UpstreamRequest,
    ) -> Pin<Box<dyn Future<Output = Result<UpstreamJsonResponse>> + Send>> {
        unreachable!("opencode-go not used in retry test")
    }

    fn opencode_go_chat_stream(
        &self,
        _request: UpstreamRequest,
    ) -> Pin<Box<dyn Future<Output = Result<UpstreamSseResponse>> + Send>> {
        unreachable!("opencode-go stream not used in retry test")
    }

    fn cursor_responses(
        &self,
        _request: UpstreamRequest,
    ) -> Pin<Box<dyn Future<Output = Result<UpstreamJsonResponse>> + Send>> {
        unreachable!("cursor not used in retry test")
    }

    fn cursor_responses_stream(
        &self,
        _request: UpstreamRequest,
    ) -> Pin<Box<dyn Future<Output = Result<UpstreamSseResponse>> + Send>> {
        unreachable!("cursor stream not used in retry test")
    }
}

impl UpstreamClient for FakeUpstream {
    fn anthropic_messages(
        &self,
        request: UpstreamRequest,
    ) -> Pin<Box<dyn Future<Output = Result<UpstreamJsonResponse>> + Send>> {
        self.calls.lock().expect("calls lock").push(request);
        Box::pin(async {
            Ok(UpstreamJsonResponse {
                status: axum::http::StatusCode::OK,
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

    fn anthropic_messages_stream(
        &self,
        request: UpstreamRequest,
    ) -> Pin<Box<dyn Future<Output = Result<UpstreamSseResponse>> + Send>> {
        self.calls.lock().expect("calls lock").push(request);
        Box::pin(async {
            Ok(UpstreamSseResponse {
                status: axum::http::StatusCode::OK,
                body: Box::pin(futures_util::stream::iter([
                    Ok(Bytes::from_static(
                        b"event: message_start\ndata: {\"message\":{\"usage\":{\"input_tokens\":1}}}\n\n",
                    )),
                    Ok(Bytes::from_static(
                        b"event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"pong\"}}\n\n",
                    )),
                    Ok(Bytes::from_static(
                        b"event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
                    )),
                    Ok(Bytes::from_static(b"event: message_stop\ndata: {}\n\n")),
                ])),
            })
        })
    }

    fn anthropic_count_tokens(
        &self,
        request: UpstreamRequest,
    ) -> Pin<Box<dyn Future<Output = Result<UpstreamJsonResponse>> + Send>> {
        self.calls.lock().expect("calls lock").push(request);
        Box::pin(async {
            Ok(UpstreamJsonResponse {
                status: axum::http::StatusCode::OK,
                body: json!({"input_tokens": 2}),
            })
        })
    }

    fn codex_responses(
        &self,
        request: UpstreamRequest,
    ) -> Pin<Box<dyn Future<Output = Result<UpstreamJsonResponse>> + Send>> {
        self.calls.lock().expect("calls lock").push(request);
        Box::pin(async {
            Ok(UpstreamJsonResponse {
                status: axum::http::StatusCode::OK,
                body: json!({
                    "id": "resp_1",
                    "object": "response",
                    "status": "completed",
                    "model": "gpt-5.4",
                    "output": [{
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": "ok"}]
                    }],
                    "output_text": "ok",
                    "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}
                }),
            })
        })
    }

    fn codex_responses_stream(
        &self,
        request: UpstreamRequest,
    ) -> Pin<Box<dyn Future<Output = Result<UpstreamSseResponse>> + Send>> {
        self.calls.lock().expect("calls lock").push(request);
        Box::pin(async {
            Ok(UpstreamSseResponse {
                status: axum::http::StatusCode::OK,
                body: Box::pin(futures_util::stream::iter([
                    Ok(Bytes::from_static(
                        b"event: response.created\ndata: {\"response\":{\"id\":\"resp_1\",\"model\":\"gpt-5.4\"}}\n\n",
                    )),
                    Ok(Bytes::from_static(
                        b"event: response.output_text.delta\ndata: {\"delta\":\"ok\"}\n\n",
                    )),
                    Ok(Bytes::from_static(
                        b"event: response.completed\ndata: {\"response\":{\"id\":\"resp_1\",\"object\":\"response\",\"status\":\"completed\",\"model\":\"gpt-5.4\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n",
                    )),
                    Ok(Bytes::from_static(b"data: [DONE]\n\n")),
                ])),
            })
        })
    }

    fn opencode_go_chat(
        &self,
        request: UpstreamRequest,
    ) -> Pin<Box<dyn Future<Output = Result<UpstreamJsonResponse>> + Send>> {
        let model = request
            .body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        self.calls.lock().expect("calls lock").push(request);
        Box::pin(async move {
            Ok(UpstreamJsonResponse {
                status: axum::http::StatusCode::OK,
                body: json!({
                    "id": "chatcmpl_1",
                    "object": "chat.completion",
                    "model": model,
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "pong"},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                }),
            })
        })
    }

    fn opencode_go_chat_stream(
        &self,
        request: UpstreamRequest,
    ) -> Pin<Box<dyn Future<Output = Result<UpstreamSseResponse>> + Send>> {
        self.calls.lock().expect("calls lock").push(request);
        Box::pin(async {
            Ok(UpstreamSseResponse {
                status: axum::http::StatusCode::OK,
                body: Box::pin(futures_util::stream::iter([
                    Ok(Bytes::from_static(
                        b"data: {\"id\":\"chatcmpl_1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"pong\"}}]}\n\n",
                    )),
                    Ok(Bytes::from_static(
                        b"data: {\"id\":\"chatcmpl_1\",\"object\":\"chat.completion.chunk\",\"choices\":[],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1}}\n\n",
                    )),
                    Ok(Bytes::from_static(b"data: [DONE]\n\n")),
                ])),
            })
        })
    }

    fn cursor_responses(
        &self,
        _request: UpstreamRequest,
    ) -> Pin<Box<dyn Future<Output = Result<UpstreamJsonResponse>> + Send>> {
        unreachable!("cursor not used in fake upstream test")
    }

    fn cursor_responses_stream(
        &self,
        _request: UpstreamRequest,
    ) -> Pin<Box<dyn Future<Output = Result<UpstreamSseResponse>> + Send>> {
        unreachable!("cursor stream not used in fake upstream test")
    }
}

fn config(auth_dir: PathBuf) -> Config {
    Config {
        host: String::new(),
        port: 8317,
        auth_dir,
        api_keys: HashSet::from(["sk-test".to_string()]),
        body_limit: "200mb".to_string(),
        cloaking: CloakingConfig {
            cli_version: "2.1.88".to_string(),
            entrypoint: "cli".to_string(),
            codex: BTreeMap::default(),
        },
        timeouts: TimeoutConfig {
            messages_ms: 120_000,
            stream_messages_ms: 600_000,
            count_tokens_ms: 30_000,
        },
        stats_enabled: true,
        debug: DebugMode::Off,
    }
}

async fn json_response(app: axum::Router, request: axum::http::Request<Body>) -> (u16, Value) {
    let response = app.oneshot(request).await.expect("response");
    let status = response.status().as_u16();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    (status, serde_json::from_slice(&body).expect("json body"))
}

async fn raw_response(
    app: axum::Router,
    request: axum::http::Request<Body>,
) -> (u16, axum::http::HeaderMap, String) {
    let response = app.oneshot(request).await.expect("response");
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    (
        status,
        headers,
        String::from_utf8(body.to_vec()).expect("utf8 body"),
    )
}

#[tokio::test]
async fn app_auth_and_no_account_responses() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let app = create_app(config(tmp.path().to_path_buf()));

    let (status, body) = json_response(
        app.clone(),
        axum::http::Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body, json!({"status": "ok"}));

    let (status, body) = json_response(
        app.clone(),
        axum::http::Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap(),
    )
    .await;
    assert_eq!(status, 401);
    assert_eq!(body["error"]["message"], "missing API key");

    let (status, body) = json_response(
        app.clone(),
        axum::http::Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header("authorization", "Bearer wrong")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"model": "sonnet", "messages": [{"role": "user", "content": "hi"}]})
                    .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, 403);
    assert_eq!(body["error"]["message"], "invalid API key");

    let (status, body) = json_response(
        app.clone(),
        axum::http::Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header("authorization", "Bearer sk-test")
            .header("content-type", "application/json")
            .header("content-length", "1")
            .body(Body::from(
                json!({"model": "sonnet", "messages": [{"role": "user", "content": "hi"}]})
                    .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, 503);
    assert_eq!(body["error"]["type"], "no_account_for_provider");

    let (status, body) = json_response(
        app,
        axum::http::Request::builder()
            .method("POST")
            .uri("/v1/messages/count_tokens")
            .header("authorization", "Bearer sk-test")
            .header("content-type", "application/json")
            .header("content-length", "1")
            .body(Body::from(
                json!({"model": "gpt-5.4", "messages": [{"role": "user", "content": "hi"}]})
                    .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, 501);
    assert_eq!(body["error"]["provider"], "codex");
}

fn opencode_go_token() -> TokenData {
    TokenData {
        access_token: "sk-opencode-go".to_string(),
        refresh_token: String::new(),
        email: "opencode-go-acct".to_string(),
        expires_at: "9999-12-31T23:59:59Z".to_string(),
        account_uuid: String::new(),
        provider: ProviderId::OpenCodeGo,
        id_token: None,
        last_refresh_at: None,
        plan_type: None,
        cursor: None,
    }
}

fn cursor_token() -> TokenData {
    TokenData {
        access_token: "cursor-jwt".to_string(),
        refresh_token: "cursor-refresh".to_string(),
        email: "cursor-acct".to_string(),
        expires_at: "2030-01-01T00:00:00Z".to_string(),
        account_uuid: "cursor-uuid".to_string(),
        provider: ProviderId::Cursor,
        id_token: None,
        last_refresh_at: None,
        plan_type: None,
        cursor: Some(pengepul::types::CursorMeta {
            service_machine_id: Some("m".into()),
            client_version: "cli-x".into(),
            config_version: "cfg".into(),
            client_id: "cid".into(),
        }),
    }
}

#[tokio::test]
async fn app_opencode_go_chat_passes_through_with_stripped_model() {
    let tmp = tempfile::tempdir().expect("tempdir");
    save_token(tmp.path(), &opencode_go_token()).expect("save token");
    let upstream = Arc::new(FakeUpstream::default());
    let app = create_app_with_upstream(config(tmp.path().to_path_buf()), upstream.clone());

    let (status, body) = json_response(
        app,
        axum::http::Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("authorization", "Bearer sk-test")
            .header("content-type", "application/json")
            .header("content-length", "256")
            .body(Body::from(
                json!({
                    "model": "opencode-go/glm-5.1",
                    "messages": [{"role": "user", "content": "hi"}]
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(body["choices"][0]["message"]["content"], "pong");
    let calls = upstream.calls();
    assert_eq!(calls.len(), 1);
    // the routing prefix is stripped before the upstream call.
    assert_eq!(calls[0].body["model"], "glm-5.1");
}

#[tokio::test]
async fn app_opencode_go_chat_streams_through() {
    let tmp = tempfile::tempdir().expect("tempdir");
    save_token(tmp.path(), &opencode_go_token()).expect("save token");
    let upstream = Arc::new(FakeUpstream::default());
    let app = create_app_with_upstream(config(tmp.path().to_path_buf()), upstream.clone());

    let (status, headers, body) = raw_response(
        app,
        axum::http::Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("authorization", "Bearer sk-test")
            .header("content-type", "application/json")
            .header("content-length", "256")
            .body(Body::from(
                json!({
                    "model": "opencode-go/glm-5.1",
                    "stream": true,
                    "messages": [{"role": "user", "content": "hi"}]
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(
        headers
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream; charset=utf-8")
    );
    assert!(body.contains("chat.completion.chunk"));
    assert!(body.contains("\"content\":\"pong\""));
    assert!(body.contains("data: [DONE]"));
    let calls = upstream.calls();
    // pengepul injects stream_options.include_usage so usage reaches accounting.
    assert_eq!(calls[0].body["stream_options"]["include_usage"], true);
    assert_eq!(calls[0].body["model"], "glm-5.1");
}

#[tokio::test]
async fn app_opencode_go_rejects_non_chat_routes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    save_token(tmp.path(), &opencode_go_token()).expect("save token");
    let upstream = Arc::new(FakeUpstream::default());
    let app = create_app_with_upstream(config(tmp.path().to_path_buf()), upstream.clone());

    let (status, body) = json_response(
        app,
        axum::http::Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header("authorization", "Bearer sk-test")
            .header("content-type", "application/json")
            .header("content-length", "256")
            .body(Body::from(
                json!({
                    "model": "opencode-go/glm-5.1",
                    "messages": [{"role": "user", "content": "hi"}]
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, 501);
    assert_eq!(body["error"]["provider"], "opencode-go");
    assert!(upstream.calls().is_empty());
}

#[tokio::test]
async fn app_opencode_go_serves_even_with_unparseable_expiry() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut token = opencode_go_token();
    token.expires_at = "not-a-real-timestamp".to_string();
    save_token(tmp.path(), &token).expect("save token");
    let upstream = Arc::new(FakeUpstream::default());
    let app = create_app_with_upstream(config(tmp.path().to_path_buf()), upstream.clone());

    let (status, body) = json_response(
        app,
        axum::http::Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("authorization", "Bearer sk-test")
            .header("content-type", "application/json")
            .header("content-length", "256")
            .body(Body::from(
                json!({
                    "model": "opencode-go/glm-5.1",
                    "messages": [{"role": "user", "content": "hi"}]
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    // opencode-go keys never refresh, so a non-RFC3339 stored expiry must not wedge the
    // account into a refresh-failure cooldown.
    assert_eq!(status, 200, "expected 200, got {status}: {body}");
    assert_eq!(body["choices"][0]["message"]["content"], "pong");
}

#[tokio::test]
async fn app_models_lists_opencode_go_when_key_present() {
    let tmp = tempfile::tempdir().expect("tempdir");
    save_token(tmp.path(), &opencode_go_token()).expect("save token");
    let app = create_app(config(tmp.path().to_path_buf()));

    let (status, body) = json_response(
        app,
        axum::http::Request::builder()
            .uri("/v1/models")
            .header("authorization", "Bearer sk-test")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, 200);
    let ids = body["data"]
        .as_array()
        .expect("data array")
        .iter()
        .filter_map(|model| model["id"].as_str())
        .collect::<Vec<_>>();
    assert!(ids.contains(&"opencode-go/glm-5.1"));
}

#[tokio::test]
async fn cursor_models_listed_when_account_present() {
    let tmp = tempfile::tempdir().expect("tempdir");
    save_token(tmp.path(), &cursor_token()).expect("save cursor token");
    let app = create_app(config(tmp.path().to_path_buf()));

    let (status, body) = json_response(
        app,
        axum::http::Request::builder()
            .uri("/v1/models")
            .header("authorization", "Bearer sk-test")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, 200);
    let ids = body["data"]
        .as_array()
        .expect("data array")
        .iter()
        .filter_map(|model| model["id"].as_str())
        .collect::<Vec<_>>();
    assert!(ids.contains(&"cursor/composer-2.5"), "{ids:?}");
}

#[tokio::test]
async fn app_models_omits_opencode_go_without_key() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let app = create_app(config(tmp.path().to_path_buf()));

    let (status, body) = json_response(
        app,
        axum::http::Request::builder()
            .uri("/v1/models")
            .header("authorization", "Bearer sk-test")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, 200);
    let lists_opencode_go = body["data"]
        .as_array()
        .expect("data array")
        .iter()
        .any(|model| {
            model["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("opencode-go/"))
        });
    assert!(
        !lists_opencode_go,
        "must not list opencode-go models without a loaded key"
    );
}

#[tokio::test]
async fn app_opencode_go_count_tokens_is_unsupported() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let app = create_app(config(tmp.path().to_path_buf()));

    let (status, body) = json_response(
        app,
        axum::http::Request::builder()
            .method("POST")
            .uri("/v1/messages/count_tokens")
            .header("authorization", "Bearer sk-test")
            .header("content-type", "application/json")
            .header("content-length", "256")
            .body(Body::from(
                json!({
                    "model": "opencode-go/glm-5.1",
                    "messages": [{"role": "user", "content": "hi"}]
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, 501);
    assert_eq!(body["error"]["provider"], "opencode-go");
}

#[tokio::test]
async fn app_opencode_go_stream_records_usage_to_account_stats() {
    let tmp = tempfile::tempdir().expect("tempdir");
    save_token(tmp.path(), &opencode_go_token()).expect("save token");
    let upstream = Arc::new(FakeUpstream::default());
    let app = create_app_with_upstream(config(tmp.path().to_path_buf()), upstream);

    let (status, _headers, _body) = raw_response(
        app.clone(),
        axum::http::Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("authorization", "Bearer sk-test")
            .header("content-type", "application/json")
            .header("content-length", "256")
            .body(Body::from(
                json!({
                    "model": "opencode-go/glm-5.1",
                    "stream": true,
                    "messages": [{"role": "user", "content": "hi"}]
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, 200);

    let (status, accounts) = json_response(
        app,
        axum::http::Request::builder()
            .uri("/admin/accounts")
            .header("authorization", "Bearer sk-test")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, 200);
    let account = &accounts["providers"]["opencode-go"]["accounts"][0];
    assert_eq!(account["totalSuccesses"], 1);
    assert_eq!(account["totalInputTokens"], 1);
    assert_eq!(account["totalOutputTokens"], 1);
}

#[tokio::test]
async fn app_enforces_configured_body_limit_and_invalid_json() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut cfg = config(tmp.path().to_path_buf());
    cfg.body_limit = "10b".to_string();
    let app = create_app(cfg);

    let (status, body) = json_response(
        app.clone(),
        axum::http::Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header("authorization", "Bearer sk-test")
            .header("content-type", "application/json")
            .header("content-length", "64")
            .body(Body::from("{}"))
            .unwrap(),
    )
    .await;
    assert_eq!(status, 413);
    assert_eq!(body["error"]["message"], "request body too large");

    let (status, body) = json_response(
        app,
        axum::http::Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header("authorization", "Bearer sk-test")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap(),
    )
    .await;
    assert_eq!(status, 411);
    assert_eq!(body["error"]["message"], "missing content-length");

    let mut cfg = config(tmp.path().to_path_buf());
    cfg.body_limit = "200mb".to_string();
    let app = create_app(cfg);
    let (status, body) = json_response(
        app,
        axum::http::Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header("authorization", "Bearer sk-test")
            .header("content-type", "application/json")
            .header("content-length", "72")
            .body(Body::from(
                b"{\"model\":\"sonnet\",\"messages\":[{\"role\":\"user\",\"content\":\"bad\njson\"}]}".to_vec(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body["error"]["message"], "invalid JSON body");
}

#[tokio::test]
async fn cors_allows_remote_origins() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let app = create_app(config(tmp.path().to_path_buf()));

    let response = app
        .oneshot(
            axum::http::Request::builder()
                .method("OPTIONS")
                .uri("/v1/messages")
                .header("origin", "https://client.example.com")
                .header("access-control-request-method", "POST")
                .header(
                    "access-control-request-headers",
                    "authorization,content-type",
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers()["access-control-allow-origin"],
        axum::http::HeaderValue::from_static("*")
    );
}

#[tokio::test]
async fn v1_routes_rate_limit_by_client() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let app = create_app(config(tmp.path().to_path_buf()));

    for _ in 0..60 {
        let (status, _) = json_response(
            app.clone(),
            axum::http::Request::builder()
                .method("GET")
                .uri("/v1/models")
                .header("authorization", "Bearer sk-test")
                .header("x-forwarded-for", "203.0.113.10")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, 200);
    }

    let (status, body) = json_response(
        app,
        axum::http::Request::builder()
            .method("GET")
            .uri("/v1/models")
            .header("authorization", "Bearer sk-test")
            .header("x-forwarded-for", "203.0.113.10")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, 429);
    assert_eq!(body["error"]["message"], "too many requests");
}

#[tokio::test]
async fn messages_route_forwards_anthropic_account_with_resolved_model() {
    let tmp = tempfile::tempdir().expect("tempdir");
    save_token(
        tmp.path(),
        &TokenData {
            access_token: "anthropic-access".to_string(),
            refresh_token: "anthropic-refresh".to_string(),
            email: "anthropic@example.com".to_string(),
            expires_at: "2030-01-01T00:00:00Z".to_string(),
            account_uuid: "acct-anthropic".to_string(),
            provider: ProviderId::Anthropic,
            id_token: None,
            last_refresh_at: None,
            plan_type: None,
            cursor: None,
        },
    )
    .expect("save token");
    let upstream = Arc::new(FakeUpstream::default());
    let app = create_app_with_upstream(config(tmp.path().to_path_buf()), upstream.clone());

    let (status, body) = json_response(
        app,
        axum::http::Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header("authorization", "Bearer sk-test")
            .header("content-type", "application/json")
            .header("content-length", "1")
            .body(Body::from(
                json!({
                    "model": "sonnet",
                    "messages": [{"role": "user", "content": "reply exactly: pong"}]
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(body["content"][0]["text"], "pong");
    let calls = upstream.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].body["model"], "claude-sonnet-4-6");
    assert_eq!(calls[0].account.token.access_token, "anthropic-access");
}

#[tokio::test]
async fn messages_route_rotates_available_anthropic_accounts() {
    let tmp = tempfile::tempdir().expect("tempdir");
    for (email, access_token) in [
        ("alice@example.com", "anthropic-access-alice"),
        ("bob@example.com", "anthropic-access-bob"),
    ] {
        save_token(
            tmp.path(),
            &TokenData {
                access_token: access_token.to_string(),
                refresh_token: format!("{access_token}-refresh"),
                email: email.to_string(),
                expires_at: "2030-01-01T00:00:00Z".to_string(),
                account_uuid: email.to_string(),
                provider: ProviderId::Anthropic,
                id_token: None,
                last_refresh_at: None,
                plan_type: None,
                cursor: None,
            },
        )
        .expect("save token");
    }
    let upstream = Arc::new(FakeUpstream::default());
    let app = create_app_with_upstream(config(tmp.path().to_path_buf()), upstream.clone());

    for _ in 0..2 {
        let (status, _) = json_response(
            app.clone(),
            axum::http::Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("authorization", "Bearer sk-test")
                .header("content-type", "application/json")
                .header("content-length", "1")
                .body(Body::from(
                    json!({
                        "model": "sonnet",
                        "messages": [{"role": "user", "content": "reply exactly: pong"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(status, 200);
    }

    let calls = upstream.calls();
    assert_eq!(calls.len(), 2);
    assert_ne!(
        calls[0].account.token.access_token,
        calls[1].account.token.access_token
    );
}

#[tokio::test]
async fn messages_route_retries_next_account_after_retryable_upstream_failure() {
    let tmp = tempfile::tempdir().expect("tempdir");
    for (email, access_token) in [
        ("alice@example.com", "anthropic-access-alice"),
        ("bob@example.com", "anthropic-access-bob"),
    ] {
        save_token(
            tmp.path(),
            &TokenData {
                access_token: access_token.to_string(),
                refresh_token: format!("{access_token}-refresh"),
                email: email.to_string(),
                expires_at: "2030-01-01T00:00:00Z".to_string(),
                account_uuid: email.to_string(),
                provider: ProviderId::Anthropic,
                id_token: None,
                last_refresh_at: None,
                plan_type: None,
                cursor: None,
            },
        )
        .expect("save token");
    }
    let upstream = Arc::new(RetryUpstream::default());
    let app = create_app_with_upstream(config(tmp.path().to_path_buf()), upstream.clone());

    let (status, body) = json_response(
        app,
        axum::http::Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header("authorization", "Bearer sk-test")
            .header("content-type", "application/json")
            .header("content-length", "1")
            .body(Body::from(
                json!({
                    "model": "sonnet",
                    "messages": [{"role": "user", "content": "reply exactly: pong"}]
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(body["content"][0]["text"], "pong");
    let calls = upstream.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls
            .iter()
            .map(|call| call.account.token.access_token.as_str())
            .collect::<Vec<_>>(),
        ["anthropic-access-alice", "anthropic-access-bob"]
    );
}

#[tokio::test]
async fn chat_completions_route_adapts_anthropic_response_to_openai() {
    let tmp = tempfile::tempdir().expect("tempdir");
    save_token(
        tmp.path(),
        &TokenData {
            access_token: "anthropic-access".to_string(),
            refresh_token: "anthropic-refresh".to_string(),
            email: "anthropic@example.com".to_string(),
            expires_at: "2030-01-01T00:00:00Z".to_string(),
            account_uuid: "acct-anthropic".to_string(),
            provider: ProviderId::Anthropic,
            id_token: None,
            last_refresh_at: None,
            plan_type: None,
            cursor: None,
        },
    )
    .expect("save token");
    let upstream = Arc::new(FakeUpstream::default());
    let app = create_app_with_upstream(config(tmp.path().to_path_buf()), upstream);

    let (status, body) = json_response(
        app,
        axum::http::Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("authorization", "Bearer sk-test")
            .header("content-type", "application/json")
            .header("content-length", "1")
            .body(Body::from(
                json!({
                    "model": "sonnet",
                    "messages": [{"role": "user", "content": "reply exactly: pong"}]
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(body["object"], "chat.completion");
    assert_eq!(body["choices"][0]["message"]["content"], "pong");
    assert_eq!(body["usage"]["prompt_tokens"], 1);
}

#[tokio::test]
async fn chat_completions_route_streams_anthropic_response_to_openai() {
    let tmp = tempfile::tempdir().expect("tempdir");
    save_token(
        tmp.path(),
        &TokenData {
            access_token: "anthropic-access".to_string(),
            refresh_token: "anthropic-refresh".to_string(),
            email: "anthropic@example.com".to_string(),
            expires_at: "2030-01-01T00:00:00Z".to_string(),
            account_uuid: "acct-anthropic".to_string(),
            provider: ProviderId::Anthropic,
            id_token: None,
            last_refresh_at: None,
            plan_type: None,
            cursor: None,
        },
    )
    .expect("save token");
    let upstream = Arc::new(FakeUpstream::default());
    let app = create_app_with_upstream(config(tmp.path().to_path_buf()), upstream);

    let (status, headers, body) = raw_response(
        app.clone(),
        axum::http::Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("authorization", "Bearer sk-test")
            .header("content-type", "application/json")
            .header("content-length", "1")
            .body(Body::from(
                json!({
                    "model": "sonnet",
                    "stream": true,
                    "messages": [{"role": "user", "content": "reply exactly: pong"}]
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, 200);
    assert!(
        headers["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    assert!(body.contains("\"object\":\"chat.completion.chunk\""));
    assert!(body.contains("\"content\":\"pong\""));
    assert!(body.contains("data: [DONE]"));

    let (status, body) = json_response(
        app,
        axum::http::Request::builder()
            .method("GET")
            .uri("/admin/accounts")
            .header("authorization", "Bearer sk-test")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, 200);
    let account = &body["providers"]["anthropic"]["accounts"][0];
    assert_eq!(account["totalSuccesses"], 1);
    assert_eq!(account["totalInputTokens"], 1);
    assert_eq!(account["totalOutputTokens"], 1);
}

#[tokio::test]
async fn responses_route_adapts_anthropic_response_to_responses() {
    let tmp = tempfile::tempdir().expect("tempdir");
    save_token(
        tmp.path(),
        &TokenData {
            access_token: "anthropic-access".to_string(),
            refresh_token: "anthropic-refresh".to_string(),
            email: "anthropic@example.com".to_string(),
            expires_at: "2030-01-01T00:00:00Z".to_string(),
            account_uuid: "acct-anthropic".to_string(),
            provider: ProviderId::Anthropic,
            id_token: None,
            last_refresh_at: None,
            plan_type: None,
            cursor: None,
        },
    )
    .expect("save token");
    let upstream = Arc::new(FakeUpstream::default());
    let app = create_app_with_upstream(config(tmp.path().to_path_buf()), upstream);

    let (status, body) = json_response(
        app,
        axum::http::Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer sk-test")
            .header("content-type", "application/json")
            .header("content-length", "1")
            .body(Body::from(
                json!({"model": "sonnet", "input": "reply exactly: pong"}).to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(body["object"], "response");
    assert_eq!(body["output_text"], "pong");
}

#[tokio::test]
async fn responses_route_sends_web_search_and_reasoning_to_anthropic() {
    let tmp = tempfile::tempdir().expect("tempdir");
    save_token(
        tmp.path(),
        &TokenData {
            access_token: "anthropic-access".to_string(),
            refresh_token: "anthropic-refresh".to_string(),
            email: "anthropic@example.com".to_string(),
            expires_at: "2030-01-01T00:00:00Z".to_string(),
            account_uuid: "acct-anthropic".to_string(),
            provider: ProviderId::Anthropic,
            id_token: None,
            last_refresh_at: None,
            plan_type: None,
            cursor: None,
        },
    )
    .expect("save token");
    let upstream = Arc::new(FakeUpstream::default());
    let app = create_app_with_upstream(config(tmp.path().to_path_buf()), upstream.clone());

    let (status, body) = json_response(
        app,
        axum::http::Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer sk-test")
            .header("content-type", "application/json")
            .header("content-length", "1")
            .body(Body::from(
                json!({
                    "model": "sonnet",
                    "input": "latest docs?",
                    "tools": [{"type": "web_search"}],
                    "reasoning": {"effort": "low"}
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(body["object"], "response");
    let calls = upstream.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].body["tools"],
        json!([{"type": "web_search_20250305", "name": "web_search"}])
    );
    assert_eq!(calls[0].body["thinking"]["budget_tokens"], 4096);
}

#[tokio::test]
async fn count_tokens_route_forwards_anthropic_account_with_resolved_model() {
    let tmp = tempfile::tempdir().expect("tempdir");
    save_token(
        tmp.path(),
        &TokenData {
            access_token: "anthropic-access".to_string(),
            refresh_token: "anthropic-refresh".to_string(),
            email: "anthropic@example.com".to_string(),
            expires_at: "2030-01-01T00:00:00Z".to_string(),
            account_uuid: "acct-anthropic".to_string(),
            provider: ProviderId::Anthropic,
            id_token: None,
            last_refresh_at: None,
            plan_type: None,
            cursor: None,
        },
    )
    .expect("save token");
    let upstream = Arc::new(FakeUpstream::default());
    let app = create_app_with_upstream(config(tmp.path().to_path_buf()), upstream.clone());

    let (status, body) = json_response(
        app,
        axum::http::Request::builder()
            .method("POST")
            .uri("/v1/messages/count_tokens")
            .header("authorization", "Bearer sk-test")
            .header("content-type", "application/json")
            .header("content-length", "1")
            .body(Body::from(
                json!({
                    "model": "sonnet",
                    "messages": [{"role": "user", "content": "reply exactly: pong"}]
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(body["input_tokens"], 2);
    let calls = upstream.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].body["model"], "claude-sonnet-4-6");
}

#[tokio::test]
async fn messages_route_translates_anthropic_payload_for_codex() {
    let tmp = tempfile::tempdir().expect("tempdir");
    save_token(
        tmp.path(),
        &TokenData {
            access_token: "codex-access".to_string(),
            refresh_token: "codex-refresh".to_string(),
            email: "codex@example.com".to_string(),
            expires_at: "2030-01-01T00:00:00Z".to_string(),
            account_uuid: "acct-codex".to_string(),
            provider: ProviderId::Codex,
            id_token: None,
            last_refresh_at: None,
            plan_type: None,
            cursor: None,
        },
    )
    .expect("save token");
    let upstream = Arc::new(FakeUpstream::default());
    let app = create_app_with_upstream(config(tmp.path().to_path_buf()), upstream.clone());

    let (status, body) = json_response(
        app,
        axum::http::Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header("authorization", "Bearer sk-test")
            .header("content-type", "application/json")
            .header("content-length", "1")
            .body(Body::from(
                json!({
                    "model": "gpt-5.4",
                    "messages": [{"role": "user", "content": "latest docs?"}],
                    "tools": [{
                        "type": "web_search_20250305",
                        "name": "web_search",
                        "allowed_domains": ["docs.anthropic.com"]
                    }]
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(body["type"], "message");
    assert_eq!(body["content"][0]["text"], "ok");
    let calls = upstream.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].body["tools"],
        json!([{"type": "web_search", "filters": {"allowed_domains": ["docs.anthropic.com"]}}])
    );
    assert_eq!(calls[0].body["stream"], true);
    assert_eq!(calls[0].account.token.access_token, "codex-access");
}

#[tokio::test]
async fn messages_route_forwards_anthropic_tool_choice_for_codex() {
    let tmp = tempfile::tempdir().expect("tempdir");
    save_token(
        tmp.path(),
        &TokenData {
            access_token: "codex-access".to_string(),
            refresh_token: "codex-refresh".to_string(),
            email: "codex@example.com".to_string(),
            expires_at: "2030-01-01T00:00:00Z".to_string(),
            account_uuid: "acct-codex".to_string(),
            provider: ProviderId::Codex,
            id_token: None,
            last_refresh_at: None,
            plan_type: None,
            cursor: None,
        },
    )
    .expect("save token");
    let upstream = Arc::new(FakeUpstream::default());
    let app = create_app_with_upstream(config(tmp.path().to_path_buf()), upstream.clone());

    let (status, _) = json_response(
        app,
        axum::http::Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header("authorization", "Bearer sk-test")
            .header("content-type", "application/json")
            .header("content-length", "1")
            .body(Body::from(
                json!({
                    "model": "gpt-5.4",
                    "messages": [{"role": "user", "content": "weather?"}],
                    "tools": [{
                        "name": "get_weather",
                        "description": "Get weather",
                        "input_schema": {
                            "type": "object",
                            "properties": {"city": {"type": "string"}}
                        }
                    }],
                    "tool_choice": {"type": "tool", "name": "get_weather"}
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, 200);
    let calls = upstream.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].body["tool_choice"],
        json!({"type": "function", "name": "get_weather"})
    );
    assert_eq!(calls[0].body["stream"], true);
}

#[tokio::test]
async fn chat_completions_route_adapts_codex_response_to_openai() {
    let tmp = tempfile::tempdir().expect("tempdir");
    save_token(
        tmp.path(),
        &TokenData {
            access_token: "codex-access".to_string(),
            refresh_token: "codex-refresh".to_string(),
            email: "codex@example.com".to_string(),
            expires_at: "2030-01-01T00:00:00Z".to_string(),
            account_uuid: "acct-codex".to_string(),
            provider: ProviderId::Codex,
            id_token: None,
            last_refresh_at: None,
            plan_type: None,
            cursor: None,
        },
    )
    .expect("save token");
    let upstream = Arc::new(FakeUpstream::default());
    let app = create_app_with_upstream(config(tmp.path().to_path_buf()), upstream);

    let (status, body) = json_response(
        app,
        axum::http::Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("authorization", "Bearer sk-test")
            .header("content-type", "application/json")
            .header("content-length", "1")
            .body(Body::from(
                json!({
                    "model": "gpt-5.4",
                    "messages": [{"role": "user", "content": "reply exactly: ok"}]
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(body["object"], "chat.completion");
    assert_eq!(body["choices"][0]["message"]["content"], "ok");
    assert_eq!(body["usage"]["total_tokens"], 2);
}

#[tokio::test]
async fn chat_completions_route_streams_codex_usage_to_account_stats() {
    let tmp = tempfile::tempdir().expect("tempdir");
    save_token(
        tmp.path(),
        &TokenData {
            access_token: "codex-access".to_string(),
            refresh_token: "codex-refresh".to_string(),
            email: "codex@example.com".to_string(),
            expires_at: "2030-01-01T00:00:00Z".to_string(),
            account_uuid: "acct-codex".to_string(),
            provider: ProviderId::Codex,
            id_token: None,
            last_refresh_at: None,
            plan_type: None,
            cursor: None,
        },
    )
    .expect("save token");
    let upstream = Arc::new(FakeUpstream::default());
    let app = create_app_with_upstream(config(tmp.path().to_path_buf()), upstream);

    let (status, headers, body) = raw_response(
        app.clone(),
        axum::http::Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("authorization", "Bearer sk-test")
            .header("content-type", "application/json")
            .header("content-length", "1")
            .body(Body::from(
                json!({
                    "model": "gpt-5.4",
                    "stream": true,
                    "messages": [{"role": "user", "content": "reply exactly: ok"}]
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, 200);
    assert!(
        headers["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    assert!(body.contains("\"content\":\"ok\""));
    assert!(body.contains("data: [DONE]"));

    let (status, body) = json_response(
        app,
        axum::http::Request::builder()
            .method("GET")
            .uri("/admin/accounts")
            .header("authorization", "Bearer sk-test")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, 200);
    let account = &body["providers"]["codex"]["accounts"][0];
    assert_eq!(account["totalSuccesses"], 1);
    assert_eq!(account["totalInputTokens"], 1);
    assert_eq!(account["totalOutputTokens"], 1);
}

#[tokio::test]
async fn chat_route_preserves_responses_web_search_for_codex() {
    let tmp = tempfile::tempdir().expect("tempdir");
    save_token(
        tmp.path(),
        &TokenData {
            access_token: "codex-access".to_string(),
            refresh_token: "codex-refresh".to_string(),
            email: "codex@example.com".to_string(),
            expires_at: "2030-01-01T00:00:00Z".to_string(),
            account_uuid: "acct-codex".to_string(),
            provider: ProviderId::Codex,
            id_token: None,
            last_refresh_at: None,
            plan_type: None,
            cursor: None,
        },
    )
    .expect("save token");
    let upstream = Arc::new(FakeUpstream::default());
    let app = create_app_with_upstream(config(tmp.path().to_path_buf()), upstream.clone());

    let (status, _) = json_response(
        app,
        axum::http::Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("authorization", "Bearer sk-test")
            .header("content-type", "application/json")
            .header("content-length", "1")
            .body(Body::from(
                json!({
                    "model": "gpt-5.4",
                    "messages": [{"role": "user", "content": "latest docs?"}],
                    "responses_tools": [{"type": "web_search", "search_context_size": "low"}],
                    "responses_tool_choice": "auto"
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, 200);
    let calls = upstream.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].body["tools"],
        json!([{"type": "web_search", "search_context_size": "low"}])
    );
    assert_eq!(calls[0].body["tool_choice"], "auto");
    assert_eq!(calls[0].body["stream"], true);
}

#[tokio::test]
async fn responses_route_normalizes_string_input_for_codex() {
    let tmp = tempfile::tempdir().expect("tempdir");
    save_token(
        tmp.path(),
        &TokenData {
            access_token: "codex-access".to_string(),
            refresh_token: "codex-refresh".to_string(),
            email: "codex@example.com".to_string(),
            expires_at: "2030-01-01T00:00:00Z".to_string(),
            account_uuid: "acct-codex".to_string(),
            provider: ProviderId::Codex,
            id_token: None,
            last_refresh_at: None,
            plan_type: None,
            cursor: None,
        },
    )
    .expect("save token");
    let upstream = Arc::new(FakeUpstream::default());
    let app = create_app_with_upstream(config(tmp.path().to_path_buf()), upstream.clone());

    let (status, _) = json_response(
        app,
        axum::http::Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("authorization", "Bearer sk-test")
            .header("content-type", "application/json")
            .header("content-length", "1")
            .body(Body::from(
                json!({
                    "model": "gpt-5.4",
                    "input": "reply exactly: ok",
                    "max_output_tokens": 32
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, 200);
    let calls = upstream.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].body["input"],
        json!([{"role": "user", "content": "reply exactly: ok"}])
    );
}
