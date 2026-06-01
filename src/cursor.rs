use serde_json::Value;

use crate::providers::strip_cursor_prefix;

pub(crate) const CURSOR_DEFAULT_MODEL: &str = "composer-2.5";

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
    pub varint: Option<u64>,
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
                let (v, p) = decode_varint(data, pos);
                out.push(RawField {
                    field,
                    wire,
                    bytes: None,
                    varint: Some(v),
                });
                pos = p;
            }
            2 => {
                let (len, after_len) = decode_varint(data, pos);
                pos = after_len;
                let len = usize::try_from(len).unwrap_or(usize::MAX);
                if pos + len > data.len() {
                    break;
                }
                out.push(RawField {
                    field,
                    wire,
                    bytes: Some(data[pos..pos + len].to_vec()),
                    varint: None,
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
        assert_eq!(fields.iter().find(|f| f.field == 2).unwrap().varint, Some(7));
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
}
