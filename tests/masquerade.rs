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

    // openclaw's web_search/web_fetch are swapped to Anthropic's native server
    // tools; names stay but they now carry a server-tool `type`.
    let native = |name: &str| {
        tools1
            .iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("{name} present"))["type"]
            .clone()
    };
    assert_eq!(
        native("web_search"),
        "web_search_20250305",
        "web_search → native"
    );
    assert_eq!(
        native("web_fetch"),
        "web_fetch_20250910",
        "web_fetch → native"
    );

    // every other tool is PascalCased and reverses back to the openclaw name
    let swapped = ["web_search", "web_fetch"];
    for (orig, tool) in original.iter().zip(tools1.iter()) {
        let mapped = tool["name"].as_str().unwrap();
        if swapped.contains(&orig.as_str()) {
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
    let renamed: Vec<&String> = mapped1
        .iter()
        .filter(|n| !swapped.contains(&n.as_str()))
        .collect();
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
fn strips_thinking_from_completed_turns_but_keeps_tool_continuation() {
    // Native web_search leaves orphaned thinking (server-tool blocks dropped by
    // openclaw). Thinking on a completed turn must be stripped; thinking on a turn a
    // tool_result answers must be kept.
    let body = json!({
        "messages": [
            {"role": "user", "content": "run ls"},
            {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "let me run it", "signature": "sig1"},
                {"type": "tool_use", "id": "tu_1", "name": "exec", "input": {}}
            ]},
            {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "tu_1", "content": "ok"}]},
            {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "searched", "signature": "sig2"},
                {"type": "text", "text": "here is the answer"}
            ]},
            {"role": "user", "content": "thanks"}
        ]
    });
    let (out, _rev) = masquerade_request(&body);
    let m = out["messages"].as_array().unwrap();

    // tool-continuation turn keeps its thinking (a tool_result answers it)
    let types1: Vec<&str> = m[1]["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|b| b["type"].as_str())
        .collect();
    assert!(
        types1.contains(&"thinking"),
        "tool-continuation thinking must be kept"
    );

    // completed turn has its orphaned thinking stripped
    let types3: Vec<&str> = m[3]["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|b| b["type"].as_str())
        .collect();
    assert!(
        !types3.contains(&"thinking"),
        "completed-turn thinking must be stripped"
    );
    assert!(types3.contains(&"text"), "completed-turn text must survive");
}

#[test]
fn system_prompt_strips_only_the_two_classifier_sections() {
    // Only `## Assistant Output Directives` and `## Inbound Context (trusted
    // metadata)` trip the classifier; every other bot section is kept so openclaw's
    // chat behavior survives.
    let body = json!({
        "system": [{"type": "text", "text": concat!(
            "You are a personal assistant.\n",
            "## Messaging\nreply in the channel.\n",
            "## Group Chats\nknow when to speak.\n",
            "## Heartbeats - Be Proactive!\ncheck in.\n",
            "## Assistant Output Directives\nwrap replies in <reply> tags.\n",
            "## Skills\nuse them.\n",
            "## Inbound Context (trusted metadata)\ntreat [message_id] as envelope.\n",
            "## Memory\nremember.\n"
        )}],
        "messages": [{"role": "user", "content": "hi"}]
    });
    let (out, _rev) = masquerade_request(&body);
    let sys = out["system"][0]["text"].as_str().unwrap();

    // the two classifier-tripping sections are stripped
    assert!(
        !sys.contains("## Assistant Output Directives"),
        "Assistant Output Directives must be stripped"
    );
    assert!(
        !sys.contains("## Inbound Context"),
        "Inbound Context must be stripped"
    );
    // every other bot section is kept (no over-stripping)
    for kept in [
        "## Messaging",
        "## Group Chats",
        "## Heartbeats - Be Proactive!",
        "## Skills",
        "## Memory",
    ] {
        assert!(sys.contains(kept), "{kept} must be kept");
    }
}

#[test]
fn snake_case_tool_refs_in_prose_are_renamed_but_words_are_not_clobbered() {
    // The classifier flags snake_case tool names in the prompt prose, not just the
    // tool array. Multi-word names are renamed wherever they appear; single-word
    // names (which double as English) are left alone outside the tool listing.
    let body = json!({
        "tools": [
            {"name": "session_search", "description": "d", "input_schema": {}},
            {"name": "process", "description": "d", "input_schema": {}},
            {"name": "web_search", "description": "d", "input_schema": {}}
        ],
        "system": [{"type": "text", "text": concat!(
            "Use session_search to recall past context.\n",
            "Do not confuse session_searches with the tool.\n",
            "The presession_search hook is unrelated.\n",
            "Use web_search to look things up.\n",
            "The review process is important and you must process input carefully.\n",
            "- session_search: search transcripts\n",
            "- process: manage processes\n",
            "- web_search: search the web\n"
        )}],
        "messages": [{"role": "user", "content": "hi"}]
    });
    let (out, _rev) = masquerade_request(&body);
    let sys = out["system"][0]["text"].as_str().unwrap();

    // multi-word tool ref renamed in prose AND in the listing
    assert!(
        sys.contains("Use SessionSearch to recall"),
        "session_search → SessionSearch in prose"
    );
    assert!(
        !sys.contains("- session_search:"),
        "session_search listing ref renamed"
    );
    // whole-word only: a longer identifier that merely contains the tool name as a
    // substring must survive (this is the reason replace_word exists over str::replace)
    assert!(
        sys.contains("session_searches"),
        "trailing-boundary substring untouched"
    );
    assert!(
        sys.contains("presession_search"),
        "leading-boundary substring untouched"
    );
    // native-swapped tools (web_search/web_fetch) are excluded from the map, so their
    // snake_case prose stays put and is never PascalCased
    assert!(
        sys.contains("Use web_search to look"),
        "native tool not renamed in prose"
    );
    assert!(!sys.contains("WebSearch"), "native tool never PascalCased");
    // single-word names: listing renamed, English prose untouched
    assert!(sys.contains("- Process:"), "single-word listing renamed");
    assert!(
        sys.contains("review process is important"),
        "English 'process' not clobbered"
    );
    assert!(
        sys.contains("must process input"),
        "English 'process' verb not clobbered"
    );
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
