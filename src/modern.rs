//! 2026-07-28 새 판 길 — 파이썬 SDK mcp 2.3.0 의 `_streamable_http_modern.handle_modern_request`(JSON 답 모드)와
//! `mcp/shared/inbound.py` 의 검사 사다리를 같은 순서·같은 글로 옮김.
//!
//! 손잡기가 없다: POST 하나 = JSON-RPC 요청 하나, 요청마다 `params._meta` 에 판·클라이언트 능력을 싣고, 머리
//! `MCP-Protocol-Version`·`Mcp-Method`·`Mcp-Name` 이 몸통과 같아야 한다. `Mcp-Param-*` 머리 검사는 도구 입력 꼴에
//! `x-mcp-header` 표시가 있을 때만 하는데, 우리 도구에는 없다 (자산 tools.json 시험이 지킨다) — 그래서 그 칸은 짓지 않는다.
//! 답 JSON 은 파이썬 `json.dumps(separators=(",", ":"))` 처럼 한글을 \u 로 바꿔 쓴다 (ascii_json).

use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::Response;
use serde_json::{Map, Value, json};

use crate::assets;
use crate::rpc::{self, Era};
use crate::tools::{Caller, Tools};

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const INVALID_PARAMS: i64 = -32602;
const HEADER_MISMATCH: i64 = -32020;
const UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;
const VERSION_KEY: &str = "io.modelcontextprotocol/protocolVersion";
const CAPABILITIES_KEY: &str = "io.modelcontextprotocol/clientCapabilities";
const INVALID_BODY: &str = "Body must be a single JSON-RPC request or notification object";

/// SDK ERROR_CODE_HTTP_STATUS: 오류 code → HTTP 상태 (표에 없으면 200)
fn status_of(code: i64) -> StatusCode {
    match code {
        PARSE_ERROR | INVALID_REQUEST | INVALID_PARAMS | HEADER_MISMATCH | -32021 | UNSUPPORTED_PROTOCOL_VERSION => StatusCode::BAD_REQUEST,
        -32601 => StatusCode::NOT_FOUND,
        _ => StatusCode::OK,
    }
}

fn empty(status: StatusCode) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = status;
    response
}

/// 답 하나 (SDK _write): 오류면 code 로 상태를 고른다. id 가 없는 오류는 끝에 `"id":null` (SDK 가 덤프 뒤 넣는다)
fn write(message: Value) -> Response {
    let status = message
        .get("error")
        .and_then(|e| e.get("code"))
        .and_then(Value::as_i64)
        .map_or(StatusCode::OK, status_of);
    let mut response = Response::new(Body::from(ascii_json(&message)));
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response
}

fn rejection(id: Option<&Value>, code: i64, message: &str, data: Option<Value>) -> Response {
    let mut error = json!({"code": code, "message": message});
    if let Some(data) = data {
        error["data"] = data;
    }
    match id {
        Some(id) => write(json!({"jsonrpc": "2.0", "id": id, "error": error})),
        None => write(json!({"jsonrpc": "2.0", "error": error, "id": null})),
    }
}

fn unsupported(id: Option<&Value>, requested: &Value) -> Response {
    let data = json!({"supported": assets::get().modern_versions, "requested": requested});
    rejection(id, UNSUPPORTED_PROTOCOL_VERSION, "Unsupported protocol version", Some(data))
}

/// 머리 값 (바이트를 latin-1 로 읽는다: starlette 와 같다)
fn header_text(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get(name).map(|v| v.as_bytes().iter().map(|b| *b as char).collect())
}

/// 새 판 요청 하나. 부르기 전에 몸통 상한(64 KiB)은 http.rs 가 봤다.
pub async fn serve(tools: &Tools, method: &Method, headers: &HeaderMap, bytes: &Bytes, caller: &Caller) -> Response {
    if let Some(refused) = crate::http::security(headers, *method == Method::POST) {
        return refused;
    }
    if *method != Method::POST {
        let mut response = empty(StatusCode::METHOD_NOT_ALLOWED);
        response.headers_mut().insert(header::ALLOW, HeaderValue::from_static("POST"));
        return response;
    }
    let (has_json, has_sse) = crate::http::accepts(headers);
    if !has_json {
        return empty(StatusCode::NOT_ACCEPTABLE);
    }
    let Ok(body) = serde_json::from_slice::<Value>(bytes) else {
        return rejection(None, PARSE_ERROR, "Parse error", None);
    };
    // 알림 꼴 (객체인데 "id" 가 없다): 받고 버린다 — 판 머리만 본다
    if let Value::Object(map) = &body
        && !map.contains_key("id")
    {
        if !notification_shaped(map) {
            return rejection(None, INVALID_REQUEST, INVALID_BODY, None);
        }
        let requested = header_text(headers, "mcp-protocol-version").unwrap_or_default();
        if !assets::get().modern_versions.contains(&requested) {
            return unsupported(None, &Value::String(requested));
        }
        return empty(StatusCode::ACCEPTED);
    }
    let Some((id, rpc_method, params)) = request_parts(&body) else {
        return rejection(None, INVALID_REQUEST, INVALID_BODY, None);
    };
    if rpc_method == "subscriptions/listen" && !has_sse {
        return empty(StatusCode::NOT_ACCEPTABLE);
    }
    for name in ["mcp-protocol-version", "mcp-method", "mcp-name"] {
        if headers.get_all(name).iter().count() > 1 {
            return rejection(Some(&id), HEADER_MISMATCH, &format!("{name} header appears more than once"), None);
        }
    }
    if let Some(refused) = ladder(&id, &rpc_method, params.as_ref(), headers) {
        return refused;
    }
    let reply = rpc::answer(tools, id, &rpc_method, params, caller, Era::Modern).await;
    write(reply)
}

/// JSONRPCNotification: jsonrpc "2.0", method 글, params 는 객체이거나 없음
fn notification_shaped(map: &Map<String, Value>) -> bool {
    map.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
        && map.get("method").is_some_and(Value::is_string)
        && matches!(map.get("params"), None | Some(Value::Null | Value::Object(_)))
}

/// JSONRPCRequest: jsonrpc "2.0", id 글 또는 정수, method 글, params 객체이거나 없음 → (id, method, params)
fn request_parts(body: &Value) -> Option<(Value, String, Option<Value>)> {
    let map = body.as_object()?;
    if map.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return None;
    }
    let id = match map.get("id")? {
        Value::String(s) => Value::String(s.clone()),
        Value::Number(n) if !crate::pyrepr::is_float_text(&n.to_string()) => Value::Number(n.clone()),
        _ => return None,
    };
    let method = map.get("method")?.as_str()?.to_string();
    let params = match map.get("params") {
        None | Some(Value::Null) => None,
        Some(Value::Object(p)) => Some(Value::Object(p.clone())),
        Some(_) => return None,
    };
    Some((id, method, params))
}

/// SDK classify_inbound_request: 봉투 → 머리 셋 → 판이 글인가 → 받는 판인가. 처음 걸린 칸의 오류를 낸다.
fn ladder(id: &Value, method: &str, params: Option<&Value>, headers: &HeaderMap) -> Option<Response> {
    let Some(meta) = params.and_then(|p| p.get("_meta")).and_then(Value::as_object) else {
        return Some(rejection(
            Some(id),
            INVALID_PARAMS,
            &format!("params._meta must be an object carrying the required '{VERSION_KEY}' and '{CAPABILITIES_KEY}' envelope keys"),
            None,
        ));
    };
    let missing: Vec<&str> = [VERSION_KEY, CAPABILITIES_KEY]
        .into_iter()
        .filter(|k| !meta.contains_key(*k))
        .collect();
    if !missing.is_empty() {
        let message = format!("params._meta is missing the required envelope key(s): {}", missing.join(", "));
        return Some(rejection(Some(id), INVALID_PARAMS, &message, None));
    }
    let version = &meta[VERSION_KEY];
    if header_text(headers, "mcp-protocol-version").map(Value::String).as_ref() != Some(version) {
        let message = "mcp-protocol-version header does not match the request envelope's protocol version";
        return Some(rejection(Some(id), HEADER_MISMATCH, message, None));
    }
    if header_text(headers, "mcp-method").as_deref() != Some(method) {
        return Some(rejection(
            Some(id),
            HEADER_MISMATCH,
            "mcp-method header does not match the request body's method",
            None,
        ));
    }
    let name_key = match method {
        "tools/call" | "prompts/get" => Some("name"),
        "resources/read" => Some("uri"),
        _ => None,
    };
    if let Some(key) = name_key
        && let Some(body_value) = params.and_then(|p| p.get(key)).filter(|v| !v.is_null())
        && header_text(headers, "mcp-name")
            .and_then(|h| decode_header_value(&h))
            .map(Value::String)
            .as_ref()
            != Some(body_value)
    {
        let message = format!("mcp-name header does not match the request body's '{key}' parameter");
        return Some(rejection(Some(id), HEADER_MISMATCH, &message, None));
    }
    let Some(version) = version.as_str() else {
        return Some(rejection(
            Some(id),
            INVALID_PARAMS,
            "the protocol-version envelope value must be a string",
            None,
        ));
    };
    if !assets::get().modern_versions.iter().any(|v| v == version) {
        return Some(unsupported(Some(id), &Value::String(version.to_string())));
    }
    None
}

/// SDK decode_header_value: `=?base64?…?=` 이면 표준 base64 를 풀어 UTF-8 (다시 쓴 꼴이 같아야 한다), 틀리면 None. 아니면 그대로
pub fn decode_header_value(value: &str) -> Option<String> {
    let Some(payload) = value.strip_prefix("=?base64?").and_then(|rest| rest.strip_suffix("?=")) else {
        return Some(value.to_string());
    };
    let bytes = base64_decode(payload)?;
    if base64_encode(&bytes) != payload {
        return None;
    }
    String::from_utf8(bytes).ok()
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8) | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// 파이썬 base64.b64decode(validate=True): 표준 글자만, 길이 4의 배수, '=' 는 끝에만
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::new();
    for (n, chunk) in bytes.chunks(4).enumerate() {
        let last = n == bytes.len() / 4 - 1;
        let pad = chunk.iter().rev().take_while(|b| **b == b'=').count();
        if pad > 2 || (pad > 0 && !last) {
            return None;
        }
        let mut value = 0u32;
        for b in &chunk[..4 - pad] {
            value = (value << 6) | ALPHABET.iter().position(|a| a == b)? as u32;
        }
        value <<= 6 * pad as u32;
        let decoded = [(value >> 16) as u8, (value >> 8) as u8, value as u8];
        out.extend_from_slice(&decoded[..3 - pad]);
    }
    Some(out)
}

/// 파이썬 `json.dumps(x, separators=(",", ":"))` (ensure_ascii 기본): 한글 등 ASCII 밖 글자를 \uXXXX(넘으면 대리 쌍)로
pub fn ascii_json(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    let mut serializer = serde_json::Serializer::with_formatter(&mut out, AsciiFormatter);
    serde::Serialize::serialize(value, &mut serializer).expect("JSON 쓰기");
    out
}

struct AsciiFormatter;

impl serde_json::ser::Formatter for AsciiFormatter {
    fn write_string_fragment<W: ?Sized + std::io::Write>(&mut self, writer: &mut W, fragment: &str) -> std::io::Result<()> {
        for c in fragment.chars() {
            if c.is_ascii() {
                writer.write_all(&[c as u8])?;
            } else {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    write!(writer, "\\u{unit:04x}")?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_tools_have_no_mcp_param_headers() {
        // Mcp-Param-* 머리 검사를 짓지 않은 근거 (맨 위 설명): 도구 입력 꼴에 x-mcp-header 가 생기면 여기서 실패한다
        assert!(!assets::get().tools.to_string().contains("x-mcp-header"));
    }

    #[test]
    fn base64_sentinel_like_python() {
        assert_eq!(decode_header_value("get_item").as_deref(), Some("get_item"));
        assert_eq!(decode_header_value("=?base64?Z2V0X2l0ZW0=?=").as_deref(), Some("get_item"));
        assert_eq!(decode_header_value("=?base64?7ZWc6riA?=").as_deref(), Some("한글"));
        assert_eq!(decode_header_value("=?base64?!!?="), None);
        assert_eq!(decode_header_value("=?base64?Z2V0X2l0ZW1=?="), None); // 다시 쓴 꼴이 다르다 (비표준 끝 비트)
    }

    #[test]
    fn ascii_json_like_python() {
        let text = String::from_utf8(ascii_json(&json!({"a": "가😀\n\"", "b": [1, null]}))).unwrap();
        assert_eq!(text, r#"{"a":"\uac00\ud83d\ude00\n\"","b":[1,null]}"#);
    }
}
