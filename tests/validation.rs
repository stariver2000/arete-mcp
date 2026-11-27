//! 인자 검사가 파이썬 SDK(mcp 2.3.0·pydantic 2.14)와 한 글자까지 같다 — 정답은 파이썬 SDK 에서 뽑은 파일 (data/).
//! 파이썬 SDK·도구 인자를 바꾸면 정답을 다시 뽑는다 (assets/ 와 같은 규칙).

use arete_mcp_rs::assets;
use arete_mcp_rs::validate::{Arg, validate};
use serde_json::{Map, Value};

fn arg_json(arg: &Arg) -> Value {
    match arg {
        Arg::Str(s) => Value::String(s.clone()),
        Arg::Int(digits) => Value::Number(digits.parse().expect("정수 글")),
        Arg::Bool(b) => Value::Bool(*b),
        Arg::Null => Value::Null,
    }
}

#[test]
fn validation_matches_the_python_sdk() {
    let text = include_str!("data/validation_cases.jsonl");
    let mut failures = Vec::new();
    let mut total = 0;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        total += 1;
        let case: Value = serde_json::from_str(line).expect("한 줄");
        let tool = case["tool"].as_str().unwrap();
        let arguments: Map<String, Value> = case["arguments"].as_object().unwrap().clone();
        let got = match assets::get().specs.iter().find(|s| s.name == tool) {
            None => serde_json::json!({"error": format!("Unknown tool: {tool}")}),
            Some(spec) => match validate(spec, &arguments) {
                Ok(values) => serde_json::json!({"ok": values.iter().map(|(k, v)| (k.clone(), arg_json(v))).collect::<Map<_, _>>()}),
                Err(message) => serde_json::json!({"error": format!("Error executing tool {tool}: {message}")}),
            },
        };
        if got != case["expected"] {
            failures.push(format!("{}\n  want {}\n  got  {}", case["arguments"], case["expected"], got));
        }
    }
    assert!(
        failures.is_empty(),
        "{} / {total} 다름:\n{}",
        failures.len(),
        failures.iter().take(60).cloned().collect::<Vec<_>>().join("\n")
    );
}
