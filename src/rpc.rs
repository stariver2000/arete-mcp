//! JSON-RPC 처리 (세션 없음·JSON 답) — 파이썬 SDK mcp 2.3.0 의 stateless_http·json_response 가 내는 것과 같게.
//!
//! rmcp 를 쓰지 않는 까닭: 이 서버가 맡는 메서드는 몇 개뿐이고, 오류·검사 글을 파이썬 SDK 와 같게 우리가 정해야 한다.
//! 메서드: initialize·ping·tools/list·tools/call·prompts/list·resources/list·resources/templates/list. 그 밖은 Method not found.

use serde_json::{Map, Value, json};

use crate::assets;
use crate::tools::{Caller, Outcome, Tools};
use crate::validate::LineError;

/// 들어온 몸통 하나의 갈래 (SDK 의 JSONRPCRequest | Notification | Response | Error 합집합 검사)
pub enum Message {
    Request {
        id: Value,
        method: String,
        params: Option<Value>,
    },
    /// 알림·응답·오류: 202 로 받고 답하지 않는다
    NoReply,
    /// 합집합 검사 실패: HTTP 400, 글은 `Validation error: …`
    Invalid(String),
}

const INVALID_PARAMS: i64 = -32602;
const METHOD_NOT_FOUND: i64 = -32601;

pub fn classify(body: &Value) -> Message {
    let Value::Object(map) = body else {
        return Message::Invalid(not_a_dict(body));
    };
    // pydantic '똑똑한' 합집합: 맞는 모델 가운데 채운 칸이 가장 많은 것 (ErrorData 안의 칸도 센다), 같으면 앞 모델
    // (Request → Notification → Response → Error). 그래서 method 와 맞는 error 가 같이 오면 Error 다 (정답 파일에서 확인)
    let per_model = model_errors(map);
    let chosen = (0..4)
        .filter(|m| per_model[*m].is_empty())
        .max_by_key(|m| (fields_set(map, *m), std::cmp::Reverse(*m)));
    match chosen {
        Some(0) => {
            let method = map["method"].as_str().unwrap_or_default().to_string();
            let params = map.get("params").filter(|p| !p.is_null()).cloned();
            return Message::Request {
                id: map["id"].clone(),
                method,
                params,
            };
        }
        Some(_) => return Message::NoReply, // 알림·응답·오류는 202 로 받고 답하지 않는다
        None => {}
    }
    let errors: Vec<LineError> = per_model.into_iter().flatten().collect();
    Message::Invalid(crate::validate::render(
        "union[JSONRPCRequest,JSONRPCNotification,JSONRPCResponse,JSONRPCError]",
        &errors,
    ))
}

/// RequestId = int | str, 엄격 (SDK mcp_types: 1.0·참·null 은 아니다 → 그런 id 의 요청은 알림으로 맞는다, 202)
fn request_id(value: &Value) -> Option<Value> {
    match value {
        Value::String(_) => Some(value.clone()),
        Value::Number(n) if !crate::pyrepr::is_float_text(&n.to_string()) => Some(Value::Number(n.clone())),
        _ => None,
    }
}

/// 몸통이 객체가 아닐 때 SDK 가 내는 네 줄 (모델마다 model_type)
fn not_a_dict(body: &Value) -> String {
    let input = crate::pyrepr::truncated(&crate::pyrepr::repr(body));
    let kind = crate::pyrepr::type_name(body);
    let version = &assets::get().pydantic_version;
    let mut out = "4 validation errors for union[JSONRPCRequest,JSONRPCNotification,JSONRPCResponse,JSONRPCError]".to_string();
    for model in ["JSONRPCRequest", "JSONRPCNotification", "JSONRPCResponse", "JSONRPCError"] {
        out.push_str(&format!(
            "\n{model}\n  Input should be a valid dictionary or instance of {model} [type=model_type, input_value={input}, input_type={kind}]\n    For further information visit https://errors.pydantic.dev/{version}/v/model_type"
        ));
    }
    out
}

/// 모델 m 이 채우는 칸 수 (pydantic fields_set, 안쪽 모델 ErrorData 의 칸도 더한다)
fn fields_set(body: &Map<String, Value>, model: usize) -> usize {
    let fields: &[&str] = match model {
        0 => &["jsonrpc", "id", "method", "params"],
        1 => &["jsonrpc", "method", "params"],
        2 => &["jsonrpc", "id", "result"],
        _ => &["jsonrpc", "id", "error"],
    };
    let own = fields.iter().filter(|f| body.contains_key(**f)).count();
    let nested = match (model, body.get("error")) {
        (3, Some(Value::Object(error))) => ["code", "message", "data"].iter().filter(|f| error.contains_key(**f)).count(),
        _ => 0,
    };
    own + nested
}

/// 모델마다의 오류 (SDK 의 pydantic 합집합 오류 글과 같은 줄, 모델 순서 Request·Notification·Response·Error, 그 안은 칸 순서).
/// 규칙은 mcp_types 모델에서 (파이썬 SDK 에서 뽑은 정답과 cargo test 가 견준다): jsonrpc 는 '2.0' 글자, id 는 엄격한 int|str(Error 는 null 도),
/// method 는 글, params 는 객체이거나 null, result 는 객체, error 는 ErrorData(code 느슨한 int·message 글).
fn model_errors(body: &Map<String, Value>) -> Vec<Vec<LineError>> {
    let whole = Value::Object(body.clone());
    let mut all = Vec::new();
    for model in ["JSONRPCRequest", "JSONRPCNotification", "JSONRPCResponse", "JSONRPCError"] {
        let mut errors = Vec::new();
        let fields: &[&str] = match model {
            "JSONRPCRequest" => &["jsonrpc", "id", "method", "params"],
            "JSONRPCNotification" => &["jsonrpc", "method", "params"],
            "JSONRPCResponse" => &["jsonrpc", "id", "result"],
            _ => &["jsonrpc", "id", "error"],
        };
        for field in fields {
            let loc = format!("{model}.{field}");
            let Some(value) = body.get(*field) else {
                if *field != "params" {
                    errors.push(LineError::new(&loc, "missing", "Field required".into(), &whole));
                }
                continue;
            };
            match *field {
                "jsonrpc" if value.as_str() != Some("2.0") => {
                    errors.push(LineError::new(&loc, "literal_error", "Input should be '2.0'".into(), value));
                }
                "id" if request_id(value).is_none() && !(model == "JSONRPCError" && value.is_null()) => {
                    errors.push(LineError::new(
                        &format!("{loc}.int"),
                        "int_type",
                        "Input should be a valid integer".into(),
                        value,
                    ));
                    errors.push(LineError::new(
                        &format!("{loc}.str"),
                        "string_type",
                        "Input should be a valid string".into(),
                        value,
                    ));
                }
                "method" if !value.is_string() => {
                    errors.push(LineError::new(&loc, "string_type", "Input should be a valid string".into(), value));
                }
                "params" if !matches!(value, Value::Null | Value::Object(_)) => {
                    errors.push(LineError::new(
                        &loc,
                        "dict_type",
                        "Input should be a valid dictionary".into(),
                        value,
                    ));
                }
                "result" if !value.is_object() => {
                    errors.push(LineError::new(
                        &loc,
                        "dict_type",
                        "Input should be a valid dictionary".into(),
                        value,
                    ));
                }
                "error" => error_data_errors(&loc, value, &mut errors),
                _ => {}
            }
        }
        all.push(errors);
    }
    all
}

fn error_data_errors(loc: &str, value: &Value, errors: &mut Vec<LineError>) {
    let Value::Object(error) = value else {
        let message = "Input should be a valid dictionary or instance of ErrorData".to_string();
        errors.push(LineError::new(loc, "model_type", message, value));
        return;
    };
    match error.get("code") {
        None => errors.push(LineError::new(&format!("{loc}.code"), "missing", "Field required".into(), value)),
        Some(code) => {
            if let Err((kind, message)) = crate::validate::check_int(code, None, None) {
                errors.push(LineError::new(&format!("{loc}.code"), kind, message, code));
            }
        }
    }
    match error.get("message") {
        None => errors.push(LineError::new(&format!("{loc}.message"), "missing", "Field required".into(), value)),
        Some(message) => {
            if let Err((kind, text)) = crate::validate::check_str(message, None, None) {
                errors.push(LineError::new(&format!("{loc}.message"), kind, text, message));
            }
        }
    }
}

/// 어느 판의 길로 왔나: 손잡기 판(initialize 로 붙는 2024-11-05~2025-11-25) 또는 새 판(2026-07-28, 요청마다 _meta 봉투)
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Era {
    Handshake,
    Modern,
}

/// 요청 하나의 답 (JSON-RPC 응답 객체). 새 판의 오류 code 는 HTTP 상태를 정한다 (http.rs modern_status)
pub async fn answer(tools: &Tools, id: Value, method: &str, params: Option<Value>, caller: &Caller, era: Era) -> Value {
    match handle(tools, method, params, caller, era).await {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err(error) => json!({"jsonrpc": "2.0", "id": id, "error": error}),
    }
}

fn invalid_params() -> Value {
    json!({"code": INVALID_PARAMS, "message": "Invalid request parameters", "data": ""})
}

fn method_not_found(method: &str) -> Value {
    json!({"code": METHOD_NOT_FOUND, "message": "Method not found", "data": method})
}

/// 새 판 결과 꾸밈 (SDK runner._serialize·2026-07-28 결과 꼴): 열쇠 이름순으로 두고 resultType complete,
/// 목록 결과는 cacheScope private·ttlMs 0 을 더하고, 끝에 _meta serverInfo 도장 (파이썬 서버가 낸 값, 자산 modern.json)
fn modern_result(result: Value, listing: bool) -> Value {
    let Value::Object(map) = result else { return result };
    let mut fields: Vec<(String, Value)> = map.into_iter().collect();
    fields.push(("resultType".into(), json!("complete")));
    if listing {
        fields.push(("cacheScope".into(), json!("private")));
        fields.push(("ttlMs".into(), json!(0)));
    }
    fields.sort_by(|x, y| x.0.cmp(&y.0));
    let mut out: Map<String, Value> = fields.into_iter().collect();
    out.insert(
        "_meta".into(),
        json!({"io.modelcontextprotocol/serverInfo": assets::get().server_info_stamp}),
    );
    Value::Object(out)
}

async fn handle(tools: &Tools, method: &str, params: Option<Value>, caller: &Caller, era: Era) -> Result<Value, Value> {
    let a = assets::get();
    let params = params.unwrap_or_else(|| json!({}));
    let modern = era == Era::Modern;
    match method {
        // 새 판에는 손잡기·ping 이 없다 (SDK: Method not found 404). 대신 server/discover
        "initialize" | "ping" if modern => Err(method_not_found(method)),
        "server/discover" if modern => Ok(a.discover.clone()),
        "initialize" => {
            let requested = initialize_version(&params).ok_or_else(invalid_params)?;
            let negotiated = if a.handshake_versions.iter().any(|v| v == &requested) {
                requested
            } else {
                a.latest_handshake().to_string()
            };
            let mut result = a.initialize.clone();
            result["protocolVersion"] = Value::String(negotiated);
            Ok(result)
        }
        "ping" => Ok(json!({})),
        "tools/list" | "prompts/list" | "resources/list" | "resources/templates/list" => {
            if !matches!(params.get("cursor"), None | Some(Value::Null | Value::String(_))) {
                return Err(invalid_params());
            }
            let result = match method {
                "tools/list" => a.tools.clone(),
                "prompts/list" => json!({"prompts": []}),
                "resources/list" => json!({"resources": []}),
                _ => json!({"resourceTemplates": []}),
            };
            Ok(if modern { modern_result(result, true) } else { result })
        }
        // 프롬프트·리소스는 하나도 없다: SDK 가 이름을 찾지 못한 글 그대로 (손잡기 판 code 0, 새 판은 그 예외가 '내부 오류' 로 바뀐다)
        "prompts/get" => {
            let Some(Value::String(name)) = params.get("name") else {
                return Err(invalid_params());
            };
            Err(if modern {
                json!({"code": -32603, "message": "Internal server error"})
            } else {
                json!({"code": 0, "message": format!("Unknown prompt: {name}")})
            })
        }
        "resources/read" => {
            let Some(Value::String(uri)) = params.get("uri") else {
                return Err(invalid_params());
            };
            Err(json!({"code": INVALID_PARAMS, "message": format!("Unknown resource: {uri}"), "data": {"uri": uri}}))
        }
        "tools/call" => {
            let Some(Value::String(name)) = params.get("name") else {
                return Err(invalid_params());
            };
            let arguments = match params.get("arguments") {
                None | Some(Value::Null) => Map::new(),
                Some(Value::Object(map)) => map.clone(),
                Some(_) => return Err(invalid_params()),
            };
            let result = match tools.call(name, &arguments, caller).await {
                Outcome::Ok(body) => {
                    // SDK: 글 결과는 json.dumps(indent=2, ensure_ascii=False), 구조 결과는 같은 dict
                    let text = serde_json::to_string_pretty(&Value::Object(body.clone())).expect("JSON 글");
                    json!({"content": [{"text": text, "type": "text"}], "isError": false, "structuredContent": body})
                }
                Outcome::Error(text) => json!({"content": [{"text": text, "type": "text"}], "isError": true}),
            };
            Ok(if modern { modern_result(result, false) } else { result })
        }
        _ => Err(method_not_found(method)),
    }
}

/// InitializeRequestParams 검사 (protocolVersion 글·capabilities 객체·clientInfo{name,version} 글) → 요청한 판
fn initialize_version(params: &Value) -> Option<String> {
    let version = params.get("protocolVersion")?.as_str()?.to_string();
    params.get("capabilities")?.as_object()?;
    let info = params.get("clientInfo")?.as_object()?;
    info.get("name")?.as_str()?;
    info.get("version")?.as_str()?;
    Some(version)
}
