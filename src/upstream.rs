//! 공개 API(/open/v1) 부르기 — 파이썬 server._get·_fetch·_moved_to·_checked 를 그대로 옮김.
//!
//! 내부 키(ARETE_INTERNAL_TOKEN)를 머리로 넘겨 공개 API 의 IP 한도를 면제받고, 한도는 limits.rs 가 센다. 키 값은 어디에도 출력하지 않는다.
//! 리다이렉트는 따라가지 않는다 (308 item_moved 는 _get 이 한 번만 따로 따라간다).

use std::time::Duration;

use serde_json::{Map, Value};

use crate::assets;
use crate::pyrepr;

/// 도구가 실패한 꼴: SDK 가 결과 글을 어떻게 만드나 (mcp/server/mcpserver/tools/base.py).
#[derive(Debug)]
pub enum ToolFail {
    /// ToolError (우리 AreteError): `Error executing tool <이름>: <글>`
    Arete(String),
    /// 그 밖의 예외: `Error executing tool <이름>` 만 (글을 내지 않는다)
    Unexpected,
}

pub struct Upstream {
    client: reqwest::Client,
    base: String,
}

pub struct Answer {
    status: u16,
    retry_after: Option<String>,
    body: Vec<u8>,
}

impl Answer {
    fn json(&self) -> Option<Value> {
        serde_json::from_slice(&self.body).ok()
    }

    /// RFC 9457 본문의 code (글일 때만)
    fn problem_code(&self) -> Option<String> {
        self.json()?.get("code")?.as_str().map(str::to_string)
    }
}

impl Upstream {
    pub fn new(base: &str, token: Option<&str>) -> Self {
        let mut headers = reqwest::header::HeaderMap::new();
        let agent = format!("arete-mcp/{}", assets::get().version); // 파이썬 판과 같은 이름 (공개 API 쪽 셈이 갈리지 않게)
        headers.insert(reqwest::header::USER_AGENT, agent.parse().expect("user-agent"));
        if let Some(token) = token.map(str::trim).filter(|t| !t.is_empty()) {
            let mut value: reqwest::header::HeaderValue = token.parse().expect("내부 키 머리 값");
            value.set_sensitive(true);
            headers.insert("x-aijob-internal", value);
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15)) // 파이썬 httpx timeout=15.0
            .redirect(reqwest::redirect::Policy::none())
            .default_headers(headers)
            .build()
            .expect("HTTP 클라이언트");
        Self {
            client,
            base: base.trim_end_matches('/').to_string(),
        }
    }

    /// 파이썬 _get: None 값은 빼고 부른다. 308 item_moved 면 남긴 번호를 한 번 따라가고 note 를 붙인다.
    pub async fn get(&self, path: &str, params: &[(&str, Option<String>)]) -> Result<Map<String, Value>, ToolFail> {
        let clean: Vec<(&str, String)> = params.iter().filter_map(|(k, v)| v.clone().map(|v| (*k, v))).collect();
        let answer = self.fetch(path, &clean).await?;
        let Some(moved) = moved_to(&answer) else { return checked(&answer) };
        // 같은 제품의 옛 번호 (공개 API 308 item_moved, 2026-11-27). 따라간 곳이 또 308 이면 checked 가 오류로 낸다
        let old_id = path.rsplit('/').next().unwrap_or(path);
        let mut result = checked(&self.fetch(&format!("/items/{moved}"), &clean).await?)?;
        result.insert(
            "note".into(),
            Value::String(format!(
                "번호 {old_id} 는 같은 항목의 옛 번호라 지금 번호 {moved} 를 냅니다. / Item {old_id} is an old number for the same item; showing current item {moved}."
            )),
        );
        Ok(result)
    }

    /// 계정 키 확인 (파이썬 server._check_remote): POST /v1/internal/mcp-keys/check {key}. 닿지 않거나 본문이 JSON 이 아니면 (0, {}).
    /// 본문은 JSON·문제 본문일 때만 읽고 객체가 아니면 {} (파이썬과 같다). 키는 본문으로만 보낸다 (주소·기록에 남지 않게)
    pub async fn check_key(&self, key: String) -> (u16, Value) {
        let url = format!("{}/v1/internal/mcp-keys/check", self.base);
        let Ok(response) = self
            .client
            .post(&url)
            .header("content-type", "application/json")
            .body(serde_json::json!({"key": key}).to_string())
            .send()
            .await
        else {
            return (0, Value::Object(Map::new()));
        };
        let status = response.status().as_u16();
        let json_like = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.starts_with("application/json") || ct.starts_with("application/problem"));
        let Ok(bytes) = response.bytes().await else {
            return (0, Value::Object(Map::new()));
        };
        if !json_like {
            return (status, Value::Object(Map::new()));
        }
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(Value::Object(body)) => (status, Value::Object(body)),
            Ok(_) => (status, Value::Object(Map::new())),
            Err(_) => (0, Value::Object(Map::new())), // 파이썬: ValueError → (0, {})
        }
    }

    /// 쓴 양 보내기 (파이썬 server._send_usage): POST /v1/internal/mcp-keys/usage, 204 면 보낸 것
    pub async fn send_usage(&self, rows: &Value) -> bool {
        let url = format!("{}/v1/internal/mcp-keys/usage", self.base);
        let sent = self
            .client
            .post(&url)
            .header("content-type", "application/json")
            .body(rows.to_string())
            .send()
            .await;
        sent.is_ok_and(|response| response.status().as_u16() == 204)
    }

    async fn fetch(&self, path: &str, params: &[(&str, String)]) -> Result<Answer, ToolFail> {
        let mut url = format!("{}/open/v1{path}", self.base);
        if !params.is_empty() {
            let query: Vec<String> = params
                .iter()
                .map(|(k, v)| format!("{}={}", form_encode(k), form_encode(v)))
                .collect();
            url = format!("{url}?{}", query.join("&"));
        }
        let unreachable = || {
            let a = assets::get();
            ToolFail::Arete(format!(
                "AI 뭐 쓰지? 서버에 잠시 닿지 않습니다. {s}초 뒤에 다시 시도하세요. / {en}; retry in {s}s. (upstream_unavailable)",
                s = a.upstream_retry_seconds,
                en = a.error_en(Some("upstream_unavailable"))
            ))
        };
        // 파이썬 httpx.TransportError(연결 거절·시간 초과·끊김) → upstream_unavailable. 본문 읽기도 같은 묶음이다
        let response = self.client.get(&url).send().await.map_err(|_| unreachable())?;
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let body = response.bytes().await.map_err(|_| unreachable())?.to_vec();
        Ok(Answer { status, retry_after, body })
    }
}

fn moved_to(answer: &Answer) -> Option<String> {
    if answer.status != 308 || answer.problem_code().as_deref() != Some("item_moved") {
        return None;
    }
    let target = answer.json()?.get("moved_to")?.get("item_id")?.clone();
    match target {
        Value::Number(n) if !pyrepr::is_float_text(&n.to_string()) => Some(pyrepr::int_text(&n.to_string())),
        _ => None,
    }
}

/// 파이썬 _checked: 300 이상이면 우리 오류 꼴, 아니면 본문 (dict 가 아니면 SDK 가 '알 수 없는 오류' 로 낸다).
fn checked(answer: &Answer) -> Result<Map<String, Value>, ToolFail> {
    if answer.status >= 300 {
        let detail = match answer.json() {
            None => String::new(),
            Some(Value::Object(body)) => match body.get("detail") {
                None => String::new(),
                Some(Value::String(s)) => s.clone(),
                Some(other) => py_str(other),
            },
            Some(_) => return Err(ToolFail::Unexpected), // 파이썬: list.get → AttributeError
        };
        if answer.status == 429 {
            let wait = answer.retry_after.clone().unwrap_or_else(|| "잠시".to_string());
            return Err(ToolFail::Arete(format!(
                "요청 한도를 넘었습니다. {wait}초 뒤에 다시 시도하세요. / Rate limit reached; retry in {wait}s."
            )));
        }
        let code = answer.problem_code();
        let tail = code.clone().unwrap_or_else(|| answer.status.to_string());
        let message = format!("{} {detail} / {} ({tail})", answer.status, assets::get().error_en(code.as_deref()));
        return Err(ToolFail::Arete(message.trim_matches(crate::text::is_py_space).to_string()));
    }
    match answer.json() {
        Some(Value::Object(body)) => Ok(body),
        _ => Err(ToolFail::Unexpected),
    }
}

/// 파이썬 f-string 의 str(x) (글이 아닌 detail): 숫자·True·None 은 repr 과 같다
fn py_str(value: &Value) -> String {
    pyrepr::repr(value)
}

/// application/x-www-form-urlencoded (httpx 가 params 를 쓰는 꼴: 공백은 +)
fn form_encode(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
