//! HTTP 겉 (axum) — 파이썬 SDK 의 streamable HTTP(세션 없음·JSON 답)·starlette 길과 같은 상태·머리·글.
//!
//! 검사 순서는 SDK 와 같다: 몸통 상한 64 KiB → MCP-Protocol-Version 이 손잡기 판이 아니면 새 판 길 → Content-Type(POST) →
//! Host·Origin (DNS rebinding) → 메서드별 (POST: Accept → Content-Type 엄격 → JSON → JSON-RPC 합집합 → 처리).
//! 127.0.0.1 에만 연다: X-Forwarded-For 는 앞의 역방향 프록시만 넣을 수 있어야 사용자 IP 한도가 맞다.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::assets;
use crate::rpc::{self, Message};
use crate::tools::{Caller, Tools};

pub const MCP_PATH: &str = "/connect/mcp";
const MCP_PATH_SLASH: &str = "/connect/mcp/";
const CARD_PATH: &str = "/connect/mcp/server-card";
const WELL_KNOWN_CARD: &str = "/.well-known/mcp/server-card.json"; // 옛 초안(SEP-1649) 클라이언트용, 같은 내용
const CATALOG_PATH: &str = "/.well-known/ai-catalog.json";
/// 받는 몸통 상한. 도구 인자는 짧다 (검색 글 100자): SDK 기본 4 MiB 대신 64 KiB (보안 점검 2026-11-27 #6, 파이썬 판 _NoStream 도 같은 상한)
const MAX_BODY: usize = 64 * 1024;
/// 몸통을 다 받기까지 (느리게 흘려 연결을 쥐는 것을 끊는다) 와 한 요청 전체 (위 API 15초 + 다시 한 번 5초 + 15초 보다 넉넉히).
/// 머리 읽기 시간 제한은 두지 않는다: 이 서버는 127.0.0.1 에만 열고, 앞의 역방향 프록시가 머리를 다 받은 뒤에야 새 요청으로 넘긴다
const BODY_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const CARD_MAX_AGE: &str = "public, max-age=3600";
const ALLOWED_HOSTS: [&str; 1] = ["arete.ai.kr"]; // + 127.0.0.1:* · localhost:* (파이썬 TransportSecuritySettings)
const ALLOWED_ORIGINS: [&str; 3] = ["https://arete.ai.kr", "https://claude.ai", "https://www.claude.ai"];

pub fn router(tools: Arc<Tools>) -> Router {
    Router::new().fallback(dispatch).with_state(tools)
}

/// 길 고르기를 손으로 한다: starlette 의 405 글·Allow 머리를 같게 내려고 (axum 기본 405·404 는 몸통이 다르다). 한 요청은 REQUEST_TIMEOUT 까지
async fn dispatch(State(tools): State<Arc<Tools>>, request: Request) -> Response {
    match tokio::time::timeout(REQUEST_TIMEOUT, route(tools, request)).await {
        Ok(response) => response,
        Err(_) => bare(StatusCode::GATEWAY_TIMEOUT, "Request timed out"),
    }
}

async fn route(tools: Arc<Tools>, request: Request) -> Response {
    let path = request.uri().path().to_string();
    match path.as_str() {
        // 끝 '/' 길은 되돌려 보내지 않고 바로 받는다: 되돌리기 Location 에 ?key=ajl_… 가 평문 http 주소로 실렸다 (보안 점검 2026-11-27 #2, 파이썬 판도 같다)
        MCP_PATH | MCP_PATH_SLASH => mcp(tools, request).await,
        CARD_PATH | WELL_KNOWN_CARD => only_get(&request).unwrap_or_else(|| card(request.headers())),
        CATALOG_PATH => only_get(&request).unwrap_or_else(catalog),
        _ => text(StatusCode::NOT_FOUND, "Not Found"),
    }
}

fn text(status: StatusCode, body: &'static str) -> Response {
    (status, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], body).into_response()
}

/// 머리 없는 글 답 (SDK 의 Response("…", status) — media_type 이 없어 content-type 이 붙지 않는다)
fn bare(status: StatusCode, body: &'static str) -> Response {
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    response
}

fn only_get(request: &Request) -> Option<Response> {
    if matches!(*request.method(), Method::GET | Method::HEAD) {
        return None;
    }
    let mut response = text(StatusCode::METHOD_NOT_ALLOWED, "Method Not Allowed");
    response.headers_mut().insert(header::ALLOW, HeaderValue::from_static("HEAD, GET"));
    Some(response)
}

fn card(headers: &HeaderMap) -> Response {
    let a = assets::get();
    let mut out = HeaderMap::new();
    out.insert(header::CACHE_CONTROL, HeaderValue::from_static(CARD_MAX_AGE));
    out.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    out.insert(header::ETAG, HeaderValue::from_str(&a.card_etag).expect("ETag"));
    if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(a.card_etag.as_str()) {
        return (StatusCode::NOT_MODIFIED, out).into_response();
    }
    out.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&a.card_content_type).expect("카드 content-type"),
    );
    (StatusCode::OK, out, a.card_body.clone()).into_response()
}

fn catalog() -> Response {
    let a = assets::get();
    let mut out = HeaderMap::new();
    out.insert(header::CACHE_CONTROL, HeaderValue::from_static(CARD_MAX_AGE));
    out.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    out.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&a.catalog_content_type).expect("목록표 content-type"),
    );
    (StatusCode::OK, out, a.catalog_body.clone()).into_response()
}

fn json_response(status: StatusCode, body: Option<&Value>) -> Response {
    let bytes = body.map(|b| serde_json::to_vec(b).expect("JSON")).unwrap_or_default();
    (status, [(header::CONTENT_TYPE, "application/json")], bytes).into_response()
}

/// SDK _create_error_response: id null 의 JSON-RPC 오류
fn rpc_error(status: StatusCode, code: i64, message: &str) -> Response {
    json_response(
        status,
        Some(&json!({"jsonrpc": "2.0", "id": null, "error": {"code": code, "message": message}})),
    )
}

pub(crate) fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

async fn mcp(tools: Arc<Tools>, request: Request) -> Response {
    if request.method() == Method::GET {
        // 서버가 먼저 보내는 SSE 줄은 열지 않는다: 405 + Allow: POST (MCP streamable HTTP 가 허락하는 답, 2026-07-28 판과 같은 답).
        // 세션 없는 서버라 보낼 것이 없는데 연결을 끝없이 쥐고 있었다 (보안 점검 2026-11-27 #5, 파이썬 판 _NoStream 도 같다)
        let mut response = Response::new(Body::empty());
        *response.status_mut() = StatusCode::METHOD_NOT_ALLOWED;
        response.headers_mut().insert(header::ALLOW, HeaderValue::from_static("POST"));
        return response;
    }
    let (parts, body) = request.into_parts();
    let headers = &parts.headers;
    if header_str(headers, "content-length")
        .and_then(|v| v.parse::<usize>().ok())
        .is_some_and(|n| n > MAX_BODY)
    {
        return bare(StatusCode::PAYLOAD_TOO_LARGE, "Request body too large");
    }
    let bytes = match tokio::time::timeout(BODY_TIMEOUT, axum::body::to_bytes(body, MAX_BODY)).await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(_)) => return bare(StatusCode::PAYLOAD_TOO_LARGE, "Request body too large"),
        Err(_) => return bare(StatusCode::REQUEST_TIMEOUT, "Request body timed out"),
    };
    let a = assets::get();
    if let Some(version) = header_str(headers, "mcp-protocol-version")
        && !a.handshake_versions.iter().any(|v| v == version)
    {
        return crate::modern::serve(&tools, &parts.method, headers, &bytes, &caller_of(headers, parts.uri.query())).await;
    }
    if let Some(refused) = security(headers, parts.method == Method::POST) {
        return refused;
    }
    match parts.method {
        Method::POST => post(&tools, headers, &bytes, &caller_of(headers, parts.uri.query())).await,
        Method::DELETE => rpc_error(
            StatusCode::METHOD_NOT_ALLOWED,
            -32600,
            "Method Not Allowed: Session termination not supported",
        ),
        _ => {
            let mut response = rpc_error(StatusCode::METHOD_NOT_ALLOWED, -32600, "Method Not Allowed");
            response
                .headers_mut()
                .insert(header::ALLOW, HeaderValue::from_static("GET, POST, DELETE"));
            response
        }
    }
}

/// SDK TransportSecurityMiddleware.validate_request: Content-Type(POST) → Host → Origin. 두 판의 길이 같이 쓴다
pub(crate) fn security(headers: &HeaderMap, is_post: bool) -> Option<Response> {
    if is_post && !header_str(headers, "content-type").is_some_and(|ct| ct.to_lowercase().starts_with("application/json")) {
        return Some(bare(StatusCode::BAD_REQUEST, "Invalid Content-Type header"));
    }
    if !host_allowed(header_str(headers, "host")) {
        return Some(bare(StatusCode::MISDIRECTED_REQUEST, "Invalid Host header"));
    }
    if let Some(origin) = header_str(headers, "origin").filter(|o| !o.is_empty())
        && !ALLOWED_ORIGINS.contains(&origin)
    {
        return Some(bare(StatusCode::FORBIDDEN, "Invalid Origin header"));
    }
    None
}

fn host_allowed(host: Option<&str>) -> bool {
    let Some(host) = host.filter(|h| !h.is_empty()) else { return false };
    ALLOWED_HOSTS.contains(&host) || host.starts_with("127.0.0.1:") || host.starts_with("localhost:")
}

/// SDK check_accept_headers: (json 을 받나, SSE 를 받나), */*·application/*·text/* 허락
pub(crate) fn accepts(headers: &HeaderMap) -> (bool, bool) {
    let accept = header_str(headers, "accept").unwrap_or_default();
    let types: Vec<String> = accept
        .split(',')
        .map(|t| t.trim().split(';').next().unwrap_or_default().trim().to_lowercase())
        .collect();
    let wildcard = types.iter().any(|t| t == "*/*");
    let json = wildcard || types.iter().any(|t| t == "application/json" || t == "application/*");
    let sse = wildcard || types.iter().any(|t| t == "text/event-stream" || t == "text/*");
    (json, sse)
}

/// 부른 쪽 (X-Forwarded-For·계정 키 Bearer 또는 ?key=)
fn caller_of(headers: &HeaderMap, query: Option<&str>) -> Caller {
    Caller {
        forwarded_for: header_str(headers, "x-forwarded-for").map(str::to_string),
        key: crate::keys::key_from(header_str(headers, "authorization"), query),
    }
}

async fn post(tools: &Tools, headers: &HeaderMap, bytes: &Bytes, caller: &Caller) -> Response {
    if !accepts(headers).0 {
        return rpc_error(
            StatusCode::NOT_ACCEPTABLE,
            -32600,
            "Not Acceptable: Client must accept application/json",
        );
    }
    let content_type = header_str(headers, "content-type").unwrap_or_default();
    let strict = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .split(',')
        .any(|part| part.trim() == "application/json");
    if !strict {
        return rpc_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            -32600,
            "Unsupported Media Type: Content-Type must be application/json",
        );
    }
    let body: Value = match serde_json::from_slice(bytes) {
        Ok(body) => body,
        Err(e) => return rpc_error(StatusCode::BAD_REQUEST, -32700, &format!("Parse error: {e}")),
    };
    match rpc::classify(&body) {
        Message::Invalid(message) => rpc_error(StatusCode::BAD_REQUEST, -32602, &format!("Validation error: {message}")),
        Message::NoReply => json_response(StatusCode::ACCEPTED, None),
        Message::Request { id, method, params } => {
            let reply = rpc::answer(tools, id, &method, params, caller, rpc::Era::Handshake).await;
            json_response(StatusCode::OK, Some(&reply))
        }
    }
}
