use std::collections::BTreeMap;
use std::io::Read as _;

use flate2::read::GzDecoder;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::providers::strip_cursor_prefix;
use crate::types::AvailableAccount;

pub(crate) const CURSOR_DEFAULT_MODEL: &str = "composer-2.5";
pub(crate) const CURSOR_API_BASE_URL: &str = "https://api2.cursor.sh";
pub(crate) const CURSOR_CHAT_PATH: &str = "/aiserver.v1.ChatService/StreamUnifiedChatWithTools";

pub(crate) fn encode_varint(mut value: u32, out: &mut Vec<u8>) {
    while value >= 0x80 {
        out.push(((value & 0x7f) as u8) | 0x80);
        value >>= 7;
    }
    out.push((value & 0xff) as u8);
}

pub(crate) fn encode_varint_field(field: u32, value: u32, out: &mut Vec<u8>) {
    encode_varint(field << 3, out); // wire type 0
    encode_varint(value, out);
}

pub(crate) fn encode_bytes_field(field: u32, payload: &[u8], out: &mut Vec<u8>) {
    encode_varint((field << 3) | 2, out); // wire type 2
    encode_varint(u32::try_from(payload.len()).unwrap_or(u32::MAX), out);
    out.extend_from_slice(payload);
}

pub(crate) fn connect_frame(payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(5 + payload.len());
    frame.push(0);
    frame.extend_from_slice(&u32::try_from(payload.len()).unwrap_or(u32::MAX).to_be_bytes());
    frame.extend_from_slice(payload);
    frame
}

#[derive(Debug)]
pub(crate) struct RawField {
    pub field: u32,
    pub wire: u8,
    pub bytes: Option<Vec<u8>>,
}

fn decode_varint(data: &[u8], pos: usize) -> (u64, usize) {
    let (mut value, mut shift, mut p) = (0u64, 0u32, pos);
    while p < data.len() {
        let b = data[p];
        p += 1;
        value |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            break;
        }
        shift += 7;
    }
    (value, p)
}

pub(crate) fn parse_fields(data: &[u8]) -> Vec<RawField> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < data.len() {
        let (tag, after) = decode_varint(data, pos);
        if after <= pos {
            break;
        }
        pos = after;
        let field = u32::try_from(tag >> 3).unwrap_or(0);
        let wire = (tag & 7) as u8;
        match wire {
            0 => {
                let (_v, p) = decode_varint(data, pos);
                out.push(RawField {
                    field,
                    wire,
                    bytes: None,
                });
                pos = p;
            }
            2 => {
                let (len, after_len) = decode_varint(data, pos);
                pos = after_len;
                let Ok(len) = usize::try_from(len) else {
                    break; // length that cannot fit usize is malformed input
                };
                if len > data.len() - pos {
                    break;
                }
                out.push(RawField {
                    field,
                    wire,
                    bytes: Some(data[pos..pos + len].to_vec()),
                });
                pos += len;
            }
            1 => pos += 8,
            5 => pos += 4,
            _ => break,
        }
    }
    out
}

pub(crate) fn field_bytes(fields: &[RawField], field: u32) -> Option<&[u8]> {
    fields
        .iter()
        .find(|f| f.field == field && f.wire == 2)
        .and_then(|f| f.bytes.as_deref())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    User,
    Assistant,
    System,
}

#[derive(Debug, Clone)]
pub(crate) struct ChatMessage {
    pub role: Role,
    pub content: String,
}

fn text_from_content(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| {
                p.get("text")
                    .or_else(|| p.get("input_text"))
                    .and_then(Value::as_str)
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(_) => value
            .get("text")
            .or_else(|| value.get("input_text"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    }
}

#[must_use]
pub(crate) fn normalize_model(model: &str) -> String {
    let stripped = strip_cursor_prefix(model).trim();
    if stripped.is_empty() {
        CURSOR_DEFAULT_MODEL.to_string()
    } else {
        stripped.to_string()
    }
}

/// Extract chat turns from a Responses-shaped body. System text (top-level `system`,
/// `instructions`, or role:"system"/"developer" turns) is concatenated and prepended to the
/// first user turn — Cursor has no system role.
#[must_use]
pub(crate) fn messages_from_body(body: &Value) -> Vec<ChatMessage> {
    let mut system = Vec::new();
    let mut turns: Vec<ChatMessage> = Vec::new();
    let mut push = |role: Role, text: String| {
        if text.trim().is_empty() {
            return;
        }
        match role {
            Role::System => system.push(text),
            r => turns.push(ChatMessage { role: r, content: text }),
        }
    };
    if let Some(s) = body.get("system") {
        push(Role::System, text_from_content(s));
    }
    if let Some(i) = body.get("instructions").filter(|v| !v.is_null()) {
        push(Role::System, text_from_content(i));
    }
    match body.get("input") {
        Some(Value::String(s)) => push(Role::User, s.clone()),
        Some(Value::Array(items)) => {
            for item in items {
                let role = match item.get("role").and_then(Value::as_str) {
                    Some("assistant") => Role::Assistant,
                    Some("system" | "developer") => Role::System,
                    _ => Role::User,
                };
                push(role, text_from_content(item.get("content").unwrap_or(item)));
            }
        }
        _ => {}
    }
    if let Some(Value::Array(items)) = body.get("messages") {
        for item in items {
            let role = match item.get("role").and_then(Value::as_str) {
                Some("assistant") => Role::Assistant,
                Some("system" | "developer") => Role::System,
                _ => Role::User,
            };
            push(role, text_from_content(item.get("content").unwrap_or(&Value::Null)));
        }
    }
    if !system.is_empty() {
        let prefix = system.join("\n\n");
        if let Some(first_user) = turns.iter_mut().find(|m| m.role == Role::User) {
            first_user.content = format!("{prefix}\n\n{}", first_user.content);
        } else {
            turns.insert(0, ChatMessage { role: Role::User, content: prefix });
        }
    }
    if turns.is_empty() {
        turns.push(ChatMessage { role: Role::User, content: String::new() });
    }
    turns
}

fn encode_chat_message(content: &str, role: u32, message_id: &str) -> Vec<u8> {
    let mut out = Vec::new();
    encode_bytes_field(1, content.as_bytes(), &mut out);
    encode_varint_field(2, role, &mut out);
    encode_bytes_field(13, message_id.as_bytes(), &mut out);
    encode_varint_field(47, 2, &mut out); // chat mode enum
    out
}

fn encode_message_id(message_id: &str, role: u32) -> Vec<u8> {
    let mut out = Vec::new();
    encode_bytes_field(1, message_id.as_bytes(), &mut out);
    encode_varint_field(3, role, &mut out);
    out
}

fn encode_model_msg(model: &str) -> Vec<u8> {
    let mut out = Vec::new();
    encode_bytes_field(1, model.as_bytes(), &mut out);
    encode_bytes_field(4, &[], &mut out);
    out
}

fn encode_cursor_setting() -> Vec<u8> {
    let mut unknown6 = Vec::new();
    encode_bytes_field(1, &[], &mut unknown6);
    encode_bytes_field(2, &[], &mut unknown6);
    let mut out = Vec::new();
    encode_bytes_field(1, b"cursor\\aisettings", &mut out);
    encode_bytes_field(3, &[], &mut out);
    encode_bytes_field(6, &unknown6, &mut out);
    encode_varint_field(8, 1, &mut out);
    encode_varint_field(9, 1, &mut out);
    out
}

fn encode_metadata() -> Vec<u8> {
    let mut out = Vec::new();
    encode_bytes_field(1, std::env::consts::OS.as_bytes(), &mut out);
    encode_bytes_field(2, std::env::consts::ARCH.as_bytes(), &mut out);
    encode_bytes_field(3, env!("CARGO_PKG_VERSION").as_bytes(), &mut out);
    encode_bytes_field(4, b"pengepul", &mut out);
    encode_bytes_field(5, chrono::Utc::now().to_rfc3339().as_bytes(), &mut out);
    out
}

#[must_use]
pub(crate) fn encode_cursor_chat_request(body: &Value) -> Vec<u8> {
    let model = normalize_model(body.get("model").and_then(Value::as_str).unwrap_or("cursor/"));
    let messages = messages_from_body(body);
    let conversation_id = uuid::Uuid::new_v4().to_string();
    let mut entries: Vec<(String, u32)> = Vec::new();

    let mut request = Vec::new();
    for msg in &messages {
        let role = if msg.role == Role::Assistant { 2 } else { 1 };
        let id = uuid::Uuid::new_v4().to_string();
        entries.push((id.clone(), role));
        let message = encode_chat_message(&msg.content, role, &id);
        encode_bytes_field(1, &message, &mut request);
    }
    encode_varint_field(2, 1, &mut request);
    encode_bytes_field(3, &[], &mut request);
    encode_varint_field(4, 1, &mut request);
    let model_msg = encode_model_msg(&model);
    encode_bytes_field(5, &model_msg, &mut request);
    encode_bytes_field(8, b"", &mut request);
    encode_varint_field(13, 1, &mut request);
    let setting = encode_cursor_setting();
    encode_bytes_field(15, &setting, &mut request);
    encode_varint_field(19, 1, &mut request);
    encode_bytes_field(23, conversation_id.as_bytes(), &mut request);
    let metadata = encode_metadata();
    encode_bytes_field(26, &metadata, &mut request);
    encode_varint_field(27, 1, &mut request);
    for (id, role) in &entries {
        let msg_id = encode_message_id(id, *role);
        encode_bytes_field(30, &msg_id, &mut request);
    }
    encode_varint_field(35, 0, &mut request);
    encode_varint_field(38, 0, &mut request);
    encode_varint_field(46, 2, &mut request);
    encode_bytes_field(47, b"", &mut request);
    encode_varint_field(48, 0, &mut request);
    encode_varint_field(49, 0, &mut request);
    encode_varint_field(51, 0, &mut request);
    encode_varint_field(53, 1, &mut request);
    encode_bytes_field(54, b"agent", &mut request);

    let mut wrapped = Vec::new();
    encode_bytes_field(1, &request, &mut wrapped);
    connect_frame(&wrapped)
}

pub(crate) const URL_SAFE_BASE64: &[u8] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn sha256_hex(input: &str) -> String {
    use std::fmt::Write as _;
    Sha256::digest(input.as_bytes()).iter().fold(String::new(), |mut acc, b| {
        let _ = write!(acc, "{b:02x}");
        acc
    })
}

fn jyh_encode(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let a = bytes[i];
        let b = if i + 1 < bytes.len() { bytes[i + 1] } else { 0 };
        let c = if i + 2 < bytes.len() { bytes[i + 2] } else { 0 };
        out.push(URL_SAFE_BASE64[(a >> 2) as usize] as char);
        out.push(URL_SAFE_BASE64[(((a & 3) << 4) | (b >> 4)) as usize] as char);
        if i + 1 < bytes.len() {
            out.push(URL_SAFE_BASE64[(((b & 15) << 2) | (c >> 6)) as usize] as char);
        }
        if i + 2 < bytes.len() {
            out.push(URL_SAFE_BASE64[(c & 63) as usize] as char);
        }
        i += 3;
    }
    out
}

#[must_use]
pub(crate) fn build_cursor_checksum(token: &str, machine_id: &str) -> String {
    let stable = if machine_id.is_empty() {
        sha256_hex(&format!("{token}machineId"))
    } else {
        machine_id.to_string()
    };
    let timestamp = u64::try_from(chrono::Utc::now().timestamp_millis() / 1_000_000).unwrap_or(0);
    let mut buf = [
        ((timestamp >> 40) & 0xff) as u8,
        ((timestamp >> 32) & 0xff) as u8,
        ((timestamp >> 24) & 0xff) as u8,
        ((timestamp >> 16) & 0xff) as u8,
        ((timestamp >> 8) & 0xff) as u8,
        (timestamp & 0xff) as u8,
    ];
    let mut prev: u8 = 165;
    for (i, byte) in buf.iter_mut().enumerate() {
        *byte = (*byte ^ prev).wrapping_add(u8::try_from(i % 256).unwrap_or(0));
        prev = *byte;
    }
    format!("{}{stable}", jyh_encode(&buf))
}

#[must_use]
pub(crate) fn cursor_headers(account: &AvailableAccount, _config: &Config) -> BTreeMap<String, String> {
    let token = &account.token.access_token;
    let meta = account.token.cursor.as_ref();
    let machine_id = meta.and_then(|m| m.service_machine_id.clone()).unwrap_or_else(|| {
        if account.account_uuid.is_empty() {
            account.device_id.clone()
        } else {
            account.account_uuid.clone()
        }
    });
    let client_version = meta.map_or_else(
        || crate::cursor_auth::CURSOR_DEFAULT_CLIENT_VERSION.to_string(),
        |m| {
            if m.client_version.is_empty() {
                crate::cursor_auth::CURSOR_DEFAULT_CLIENT_VERSION.to_string()
            } else {
                m.client_version.clone()
            }
        },
    );
    let config_version = meta.map_or_else(
        || uuid::Uuid::new_v4().to_string(),
        |m| {
            if m.config_version.is_empty() {
                uuid::Uuid::new_v4().to_string()
            } else {
                m.config_version.clone()
            }
        },
    );
    let session_id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, token.as_bytes()).to_string();
    let os = if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    };
    BTreeMap::from([
        ("Authorization".into(), format!("Bearer {token}")),
        ("Content-Type".into(), "application/connect+proto".into()),
        ("Accept".into(), "application/connect+proto".into()),
        ("Connect-Protocol-Version".into(), "1".into()),
        ("User-Agent".into(), "connect-es/1.6.1".into()),
        ("x-client-key".into(), sha256_hex(token)),
        ("x-cursor-checksum".into(), build_cursor_checksum(token, &machine_id)),
        ("x-cursor-client-version".into(), client_version),
        ("x-cursor-client-type".into(), "ide".into()),
        ("x-cursor-client-os".into(), os.into()),
        ("x-cursor-config-version".into(), config_version),
        ("x-ghost-mode".into(), "true".into()),
        ("x-session-id".into(), session_id),
        ("x-request-id".into(), uuid::Uuid::new_v4().to_string()),
    ])
}

#[derive(Debug, Default, Clone)]
pub(crate) struct CursorDecoded {
    pub text: String,
    pub reasoning: String,
    pub error: Option<String>,
}

struct Frame {
    kind: u8,
    payload: Vec<u8>,
}

fn read_connect_frames(data: &[u8]) -> Vec<Frame> {
    let mut frames = Vec::new();
    let mut pos = 0;
    while pos + 5 <= data.len() {
        let kind = data[pos];
        let len = u32::from_be_bytes([data[pos + 1], data[pos + 2], data[pos + 3], data[pos + 4]])
            as usize;
        pos += 5;
        if len > data.len() - pos {
            break;
        }
        let mut payload = data[pos..pos + len].to_vec();
        pos += len;
        if kind == 1 || kind == 3 {
            let mut decoded = Vec::new();
            if GzDecoder::new(&payload[..]).read_to_end(&mut decoded).is_ok() {
                payload = decoded;
            }
        }
        frames.push(Frame { kind, payload });
    }
    frames
}

fn is_printable(text: &str) -> bool {
    !text.is_empty()
        && text.chars().all(|c| {
            c == '\t' || c == '\n' || c == '\r' || (' '..='~').contains(&c) || c >= '\u{a0}'
        })
}

fn is_uuid_like(text: &str) -> bool {
    let t = text.trim();
    t.len() >= 32 && t.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

fn looks_like_proto_start(byte: u8) -> bool {
    let wire = byte & 0x07;
    byte != 0 && matches!(wire, 0 | 1 | 2 | 5)
}

fn extract_inner_text(payload: &[u8], depth: u8) -> String {
    if depth > 4 {
        return String::new();
    }
    let fields = parse_fields(payload);
    if let Some(bytes) = field_bytes(&fields, 1) {
        let text = String::from_utf8_lossy(bytes).to_string();
        if is_printable(&text) && !is_uuid_like(&text) {
            return text;
        }
    }
    let mut acc = String::new();
    for f in &fields {
        if let Some(bytes) = &f.bytes
            && bytes.len() > 1
            && looks_like_proto_start(bytes[0])
        {
            acc.push_str(&extract_inner_text(bytes, depth + 1));
        }
    }
    acc
}

fn extract_from_payload(payload: &[u8], text: &mut String, reasoning: &mut String) {
    for f in parse_fields(payload) {
        let Some(bytes) = f.bytes else { continue };
        if f.field == 25 {
            reasoning.push_str(&extract_inner_text(&bytes, 0));
        } else if f.field == 1 {
            let direct = String::from_utf8_lossy(&bytes).to_string();
            if is_printable(&direct) && !is_uuid_like(&direct) {
                text.push_str(&direct);
            }
        } else if (f.field == 2 || bytes.len() > 1)
            && bytes.first().is_some_and(|&b| looks_like_proto_start(b))
        {
            extract_from_payload(&bytes, text, reasoning);
        }
    }
}

fn extract_json_error(payload: &[u8]) -> Option<String> {
    let v: Value = serde_json::from_slice(payload).ok()?;
    let err = v.get("error")?;
    let code = err.get("code").and_then(Value::as_str);
    let message = err.get("message").and_then(Value::as_str);
    (code.is_some() || message.is_some())
        .then(|| [code, message].into_iter().flatten().collect::<Vec<_>>().join(" — "))
}

/// Byte index of the first case-insensitive match of an ASCII `needle` in `haystack`.
///
/// `needle` must be ASCII. The returned index (and `index + needle.len()`) are valid char
/// boundaries in `haystack` because every matched byte is ASCII, so slicing there never panics —
/// unlike searching a `to_lowercase()` copy, whose byte offsets need not map back to the original.
fn find_ascii_ci(haystack: &str, needle: &str) -> Option<usize> {
    let hay = haystack.as_bytes();
    let need = needle.as_bytes();
    if need.is_empty() || hay.len() < need.len() {
        return None;
    }
    (0..=hay.len() - need.len())
        .find(|&i| hay[i..i + need.len()].eq_ignore_ascii_case(need))
}

#[must_use]
pub(crate) fn decode_cursor_response(data: &[u8]) -> CursorDecoded {
    let mut out = CursorDecoded::default();
    for frame in read_connect_frames(data) {
        if frame.kind == 0 || frame.kind == 1 {
            extract_from_payload(&frame.payload, &mut out.text, &mut out.reasoning);
        } else if (frame.kind == 2 || frame.kind == 3)
            && let Some(err) = extract_json_error(&frame.payload)
        {
            out.error = Some(err);
        }
    }
    // composer/kimi: full answer follows `</think>` inside the reasoning channel.
    // Search the original string case-insensitively (not `to_lowercase()`, whose byte length can
    // differ from the original for some Unicode, yielding a non-char-boundary slice index).
    if out.text.is_empty()
        && let Some(idx) = find_ascii_ci(&out.reasoning, "</think>")
    {
        let after = idx + "</think>".len();
        out.text = out.reasoning[after..].trim_start().to_string();
        out.reasoning = out.reasoning[..idx].to_string();
    }
    out.text = out.text.trim().to_string();
    out.reasoning = out.reasoning.trim().to_string();
    out
}

#[must_use]
pub(crate) fn synth_responses_json(decoded: &CursorDecoded, model: &str) -> Value {
    json!({
        "id": format!("resp_{}", uuid::Uuid::new_v4().simple()),
        "object": "response",
        "model": model,
        "status": "completed",
        "output": [{
            "type": "message",
            "role": "assistant",
            "content": [{ "type": "output_text", "text": decoded.text }]
        }],
        "usage": { "input_tokens": 0, "output_tokens": 0, "total_tokens": 0 }
    })
}

fn sse_event(event: &str, data: &Value) -> String {
    format!("event: {event}\ndata: {data}\n\n")
}

/// Build the full Responses-API SSE sequence for one decoded response. Used by the non-incremental
/// test path and as the template for the streaming decoder's per-chunk emission.
#[must_use]
pub(crate) fn responses_sse_from_decoded(text: &str, reasoning: &str, model: &str) -> Vec<String> {
    let mut out = Vec::new();
    out.push(sse_event(
        "response.created",
        &json!({"type": "response.created",
            "response": {"id": "resp_stream", "object": "response", "model": model, "status": "in_progress", "output": []}}),
    ));
    if !reasoning.is_empty() {
        out.push(sse_event(
            "response.reasoning_text.delta",
            &json!({"type": "response.reasoning_text.delta", "delta": reasoning}),
        ));
    }
    if !text.is_empty() {
        out.push(sse_event(
            "response.output_text.delta",
            &json!({"type": "response.output_text.delta", "delta": text}),
        ));
    }
    out.push(sse_event(
        "response.completed",
        &json!({"type": "response.completed",
            "response": {"id": "resp_stream", "object": "response", "model": model, "status": "completed",
                "output": [{"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": text}]}],
                "usage": {"input_tokens": 0, "output_tokens": 0, "total_tokens": 0}}}),
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_and_field_encoding_match_known_vectors() {
        let mut v = Vec::new();
        encode_varint(300, &mut v);
        assert_eq!(v, vec![0xac, 0x02]);
        let mut b = Vec::new();
        encode_bytes_field(1, b"hi", &mut b);
        assert_eq!(b, vec![0x0a, 0x02, b'h', b'i']);
        let mut n = Vec::new();
        encode_varint_field(2, 1, &mut n);
        assert_eq!(n, vec![0x10, 0x01]);
    }

    #[test]
    fn parse_fields_round_trips_a_bytes_field() {
        let mut buf = Vec::new();
        encode_bytes_field(1, b"hello", &mut buf);
        encode_varint_field(2, 7, &mut buf);
        let fields = parse_fields(&buf);
        assert_eq!(field_bytes(&fields, 1).unwrap(), b"hello");
        // the trailing varint field is parsed (wire type 0) without corrupting the byte field
        // that precedes it.
        assert_eq!(fields.iter().find(|f| f.field == 2).unwrap().wire, 0);
    }

    #[test]
    fn folds_system_into_first_user_turn() {
        let body = serde_json::json!({
            "instructions": "be terse",
            "input": [{"role": "user", "content": "hi"}, {"role": "assistant", "content": "ok"}]
        });
        let msgs = messages_from_body(&body);
        assert_eq!(msgs[0].role, Role::User);
        assert_eq!(msgs[0].content, "be terse\n\nhi");
        assert_eq!(msgs[1].role, Role::Assistant);
        // no system role emitted
        assert!(msgs.iter().all(|m| m.role != Role::System));
    }

    #[test]
    fn normalizes_model() {
        assert_eq!(normalize_model("cursor/composer-2.5"), "composer-2.5");
        assert_eq!(normalize_model("cursor/foo"), "foo");
        assert_eq!(normalize_model("cursor/"), "composer-2.5");
    }

    #[test]
    fn encodes_chat_request_with_model_and_messages() {
        let body = serde_json::json!({"model": "cursor/composer-2.5", "input": "hello"});
        let frame = encode_cursor_chat_request(&body);
        // strip the 5-byte connect envelope
        assert_eq!(frame[0], 0);
        let len = u32::from_be_bytes([frame[1], frame[2], frame[3], frame[4]]) as usize;
        let payload = &frame[5..5 + len];
        // outer wrapper: field 1 = request message
        let outer = parse_fields(payload);
        let request = field_bytes(&outer, 1).expect("request field");
        let req_fields = parse_fields(request);
        // field 5 = model message, which contains field 1 = model name
        let model_msg = field_bytes(&req_fields, 5).expect("model field");
        let model_fields = parse_fields(model_msg);
        let model_name = field_bytes(&model_fields, 1).expect("model name");
        assert_eq!(model_name, b"composer-2.5");
    }

    #[test]
    fn checksum_ends_with_machine_id_and_uses_url_safe_alphabet() {
        let checksum = build_cursor_checksum("token-abc", "machine-1");
        assert!(checksum.ends_with("machine-1"), "{checksum}");
        let prefix = &checksum[..checksum.len() - "machine-1".len()];
        assert!(prefix.bytes().all(|b| URL_SAFE_BASE64.contains(&b)), "{prefix}");
    }

    #[test]
    fn decodes_text_from_connect_frames() {
        // inner message: field 1 = "hello"
        let mut inner = Vec::new();
        encode_bytes_field(1, b"hello", &mut inner);
        // outer stream message: field 2 = inner
        let mut outer = Vec::new();
        encode_bytes_field(2, &inner, &mut outer);
        let frame = connect_frame(&outer);
        let decoded = decode_cursor_response(&frame);
        assert_eq!(decoded.text, "hello");
    }

    #[test]
    fn decodes_gzipped_frame() {
        use flate2::{Compression, write::GzEncoder};
        use std::io::Write as _;
        let mut inner = Vec::new();
        encode_bytes_field(1, b"zipped", &mut inner);
        let mut payload = Vec::new();
        encode_bytes_field(2, &inner, &mut payload);
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&payload).unwrap();
        let gz = enc.finish().unwrap();
        // frame kind 1 = compressed
        let mut frame = vec![1u8];
        frame.extend_from_slice(&u32::try_from(gz.len()).unwrap().to_be_bytes());
        frame.extend_from_slice(&gz);
        assert_eq!(decode_cursor_response(&frame).text, "zipped");
    }

    #[test]
    fn decode_skips_empty_length_delimited_field_without_panic() {
        // A zero-length field-2 entry must not panic on bytes[0] indexing.
        let mut payload = Vec::new();
        encode_bytes_field(2, &[], &mut payload);
        encode_bytes_field(1, b"answer", &mut payload);
        let frame = connect_frame(&payload);
        assert_eq!(decode_cursor_response(&frame).text, "answer");
    }

    #[test]
    fn think_split_is_unicode_safe() {
        // 'İ' lowercases to two chars; a to_lowercase()-based index would mis-slice (or split a
        // char boundary of) the original reasoning. find_ascii_ci must split correctly.
        let mut inner = Vec::new();
        encode_bytes_field(1, "İ思考</think>done".as_bytes(), &mut inner);
        let mut payload = Vec::new();
        encode_bytes_field(25, &inner, &mut payload);
        let frame = connect_frame(&payload);
        let decoded = decode_cursor_response(&frame);
        assert_eq!(decoded.text, "done");
        assert_eq!(decoded.reasoning, "İ思考");
    }

    #[test]
    fn synthesizes_responses_json_with_output_text() {
        let decoded = CursorDecoded {
            text: "hi there".into(),
            reasoning: String::new(),
            error: None,
        };
        let payload = synth_responses_json(&decoded, "composer-2.5");
        assert_eq!(payload["object"], "response");
        let text = payload["output"][0]["content"][0]["text"].as_str().unwrap();
        assert_eq!(text, "hi there");
    }

    #[test]
    fn streaming_sse_emits_output_text_delta_and_completed() {
        let chunks = responses_sse_from_decoded("hello", "", "composer-2.5");
        let joined = chunks.join("");
        assert!(joined.contains("response.output_text.delta"), "{joined}");
        assert!(joined.contains("\"delta\":\"hello\""), "{joined}");
        assert!(joined.contains("response.completed"), "{joined}");
    }
}
