//! JSON-RPC 몸통 검사가 파이썬 SDK(mcp 2.3.0·pydantic 2.14)와 같다 — 정답은 파이썬 SDK 에서 뽑은 파일 (data/).
//! 맞은 몸통은 요청(JSONRPCRequest)인가 받고 버리는가(알림·응답·오류)를, 틀린 몸통은 오류 글을 한 글자까지 견준다.

use arete_mcp_rs::rpc::{Message, classify};
use serde_json::Value;

#[test]
fn body_classification_matches_the_python_sdk() {
    let mut failures = Vec::new();
    let mut total = 0;
    for line in include_str!("data/union_cases.jsonl").lines().filter(|l| !l.trim().is_empty()) {
        total += 1;
        let case: Value = serde_json::from_str(line).expect("한 줄");
        let want = &case["expected"];
        let got = match classify(&case["body"]) {
            Message::Request { .. } => "request".to_string(),
            Message::NoReply => "accepted".to_string(),
            Message::Invalid(text) => text,
        };
        let want_text = match want.get("ok").and_then(Value::as_str) {
            Some("JSONRPCRequest") => "request".to_string(),
            Some(_) => "accepted".to_string(),
            None => want["error"].as_str().unwrap_or_default().to_string(),
        };
        if got != want_text {
            failures.push(format!("{}\n  want {want_text}\n  got  {got}", case["body"]));
        }
    }
    assert!(
        failures.is_empty(),
        "{} / {total} 다름:\n{}",
        failures.len(),
        failures.iter().take(8).cloned().collect::<Vec<_>>().join("\n")
    );
}
