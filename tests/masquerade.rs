use std::fs;

use pengepul::masquerade::{masquerade_request, restore_tool_name};
use serde_json::{Value, json};

fn fixture() -> Value {
    let raw = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/openclaw-embedded-body.json"
    ))
    .expect("fixture present");
    serde_json::from_str(&raw).expect("fixture parses")
}

#[test]
fn tool_names_are_pascalcased_deterministically_and_bijectively() {
    let body = fixture();
    let original: Vec<String> = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();

    let (out1, rev1) = masquerade_request(&body);
    let (out2, _rev2) = masquerade_request(&body);

    let tools1 = out1["tools"].as_array().unwrap();
    let tools2 = out2["tools"].as_array().unwrap();
    let mapped1: Vec<String> = tools1
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    let mapped2: Vec<String> = tools2
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();

    // deterministic: same input → same output
    assert_eq!(mapped1, mapped2, "mapping must be deterministic");

    // openclaw's web_search is swapped to Anthropic's native server tool; its name
    // stays but it now carries the server-tool `type`.
    let ws = tools1
        .iter()
        .find(|t| t["name"] == "web_search")
        .expect("web_search present");
    assert_eq!(ws["type"], "web_search_20250305", "web_search → native");

    // every other tool is PascalCased and reverses back to the openclaw name
    for (orig, tool) in original.iter().zip(tools1.iter()) {
        let mapped = tool["name"].as_str().unwrap();
        if orig == "web_search" {
            continue;
        }
        assert_eq!(mapped, &pascal(orig), "{orig} must PascalCase to {mapped}");
        assert_eq!(
            &restore_tool_name(mapped, &rev1),
            orig,
            "reverse round-trips"
        );
    }

    // renamed names are unique (bijective)
    let renamed: Vec<&String> = mapped1.iter().filter(|n| *n != "web_search").collect();
    let uniq: std::collections::BTreeSet<_> = renamed.iter().collect();
    assert_eq!(uniq.len(), renamed.len(), "renamed names must be unique");
}

fn pascal(name: &str) -> String {
    name.split(['_', '-', ' ', '.'])
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut c = p.chars();
            c.next()
                .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                .unwrap_or_default()
        })
        .collect()
}

#[test]
fn assistant_tool_use_names_are_mapped_in_request_history() {
    let mut body = fixture();
    // inject an assistant turn that called `exec`
    body["messages"] = json!([
        {"role": "user", "content": "run ls"},
        {"role": "assistant", "content": [
            {"type": "tool_use", "id": "tu_1", "name": "exec", "input": {"cmd": "ls"}}
        ]},
        {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "tu_1", "content": "file.txt"}
        ]}
    ]);

    let (out, rev) = masquerade_request(&body);
    let tu_name = out["messages"][1]["content"][0]["name"].as_str().unwrap();
    assert_ne!(tu_name, "exec", "history tool_use name must be masked");
    // and the masked name reverses back to exec
    assert_eq!(restore_tool_name(tu_name, &rev), "exec");
    // tool_result (references id, not name) is untouched
    assert_eq!(out["messages"][2]["content"][0]["tool_use_id"], "tu_1");
}

#[test]
fn system_prompt_strips_bot_sections_and_keeps_coding_sections() {
    let body = fixture();
    let (out, _rev) = masquerade_request(&body);
    let sys: String = out["system"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|b| b["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");

    // bot sections gone
    assert!(
        !sys.contains("## Messaging"),
        "Messaging section must be stripped"
    );
    assert!(
        !sys.contains("## Group Chats"),
        "Group Chats must be stripped"
    );
    assert!(
        !sys.contains("Know When to Speak"),
        "chat-behavior must be stripped"
    );
    assert!(
        !sys.contains("Heartbeats - Be Proactive"),
        "heartbeats must be stripped"
    );
    // kept sections survive
    assert!(sys.contains("## Skills"), "Skills section must be kept");
    assert!(sys.contains("## Memory"), "Memory section must be kept");
}

#[test]
fn masquerade_leaves_persona_line_untouched() {
    let body = fixture();
    let persona = body["system"][0]["text"]
        .as_str()
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_string();

    let (out, _rev) = masquerade_request(&body);
    let sys: String = out["system"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|b| b["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");

    // The persona is an operator workspace value, not an openclaw constant, and
    // does not move the classifier. Scrubbing it was tried and dropped.
    assert!(
        sys.contains(&persona),
        "persona line must reach the upstream unchanged: {persona}"
    );
}

#[test]
fn restores_tool_use_names_in_response_body_and_sse_event() {
    let mut reverse = std::collections::BTreeMap::new();
    reverse.insert("Bash".to_string(), "exec".to_string());

    // non-streaming message body
    let mut body = json!({
        "content": [
            {"type": "text", "text": "ok"},
            {"type": "tool_use", "id": "t1", "name": "Bash", "input": {}}
        ]
    });
    pengepul::masquerade::restore_tool_use_names(&mut body, &reverse);
    assert_eq!(body["content"][1]["name"], "exec");

    // streaming content_block_start event
    let mut evt = json!({
        "type": "content_block_start",
        "index": 1,
        "content_block": {"type": "tool_use", "id": "t1", "name": "Bash", "input": {}}
    });
    pengepul::masquerade::restore_tool_use_names(&mut evt, &reverse);
    assert_eq!(evt["content_block"]["name"], "exec");

    // unknown / empty map is a no-op
    let mut untouched = json!({"content_block": {"type": "tool_use", "name": "Read"}});
    pengepul::masquerade::restore_tool_use_names(
        &mut untouched,
        &std::collections::BTreeMap::new(),
    );
    assert_eq!(untouched["content_block"]["name"], "Read");
}
