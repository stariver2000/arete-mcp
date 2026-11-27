//! 도구 인자 검사: 파이썬 SDK(mcp 2.3.0)가 pydantic 2.14 로 하는 일을 같은 글로.
//!
//! 검사 규칙은 Rust 에 다시 쓰지 않고 자산 tools.json 의 inputSchema(파이썬 서버가 낸 것)에서 읽는다: 칸 순서·필수·기본값·
//! 종류(string·integer·boolean·enum)·범위·null 허용(anyOf [X, null]).
//! 순서는 SDK 와 같다: ① pre_parse_json (칸 꼴이 str 이 아닌데 글이 오면 JSON 으로 풀어 list·dict·null 일 때만 바꿈,
//! mcp/server/mcpserver/utilities/func_metadata.py) ② pydantic 느슨한(lax) 파이썬 모드 검사 — 칸마다 오류 하나, 칸 순서대로.
//! rs/tests/validation.rs 가 파이썬 쪽 결과(rs/tests/data/validation_cases.jsonl, rs/gen_validation_cases.py)와 한 글자까지 견준다.

use serde_json::{Map, Value};

use crate::pyrepr::{self, int_text, is_float_text, type_name};

#[derive(Debug, Clone)]
pub enum Shape {
    Str { min: Option<usize>, max: Option<usize> },
    Int { min: Option<i128>, max: Option<i128> },
    Bool,
    Literal(Vec<String>),
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub shape: Shape,
    pub nullable: bool,
    pub required: bool,
    pub default: Value,
}

impl Param {
    /// 파이썬 칸 꼴이 그냥 `str` 인가 (pre_parse_json 은 이 칸만 건너뛴다. `str | None` 은 str 이 아니다).
    fn plain_str(&self) -> bool {
        matches!(self.shape, Shape::Str { .. }) && !self.nullable
    }
}

#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: String,
    /// pydantic 모델 이름 (`searchArguments`), 오류 첫 줄에 나온다
    pub model: String,
    pub params: Vec<Param>,
}

/// 검사를 지난 값. 정수는 크기 제한이 없으므로(파이썬 int) 10진 글로 둔다.
#[derive(Debug, Clone, PartialEq)]
pub enum Arg {
    Str(String),
    Int(String),
    Bool(bool),
    Null,
}

impl Arg {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Arg::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> bool {
        matches!(self, Arg::Bool(true))
    }
    pub fn as_int(&self) -> &str {
        match self {
            Arg::Int(s) => s,
            _ => "0",
        }
    }
}

pub fn spec_from_schema(tool: &Value) -> ToolSpec {
    let schema = &tool["inputSchema"];
    let required: Vec<&str> = schema["required"]
        .as_array()
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let params = schema["properties"]
        .as_object()
        .expect("inputSchema.properties")
        .iter()
        .map(|(name, prop)| {
            let (inner, nullable) = match prop.get("anyOf").and_then(Value::as_array) {
                Some(options) => (options.iter().find(|o| o["type"] != "null").expect("anyOf 안의 꼴"), true),
                None => (prop, false),
            };
            Param {
                name: name.clone(),
                shape: shape_of(inner),
                nullable,
                required: required.contains(&name.as_str()),
                default: prop.get("default").cloned().unwrap_or(Value::Null),
            }
        })
        .collect();
    ToolSpec {
        name: tool["name"].as_str().expect("name").to_string(),
        model: schema["title"].as_str().expect("title").to_string(),
        params,
    }
}

fn shape_of(prop: &Value) -> Shape {
    let usize_of = |key: &str| prop.get(key).and_then(Value::as_u64).map(|n| n as usize);
    let int_of = |key: &str| prop.get(key).and_then(Value::as_i64).map(i128::from);
    if let Some(options) = prop.get("enum").and_then(Value::as_array) {
        return Shape::Literal(options.iter().map(|o| o.as_str().expect("enum 은 글").to_string()).collect());
    }
    match prop["type"].as_str() {
        Some("string") => Shape::Str {
            min: usize_of("minLength"),
            max: usize_of("maxLength"),
        },
        Some("integer") => Shape::Int {
            min: int_of("minimum"),
            max: int_of("maximum"),
        },
        Some("boolean") => Shape::Bool,
        other => panic!("모르는 인자 꼴 {other:?}"),
    }
}

/// 검사 결과: 칸 이름 → 값 (기본값 채움), 아니면 SDK 가 내는 오류 글 (`Error executing tool …` 앞머리는 부르는 쪽이 붙인다).
pub fn validate(spec: &ToolSpec, arguments: &Map<String, Value>) -> Result<Vec<(String, Arg)>, String> {
    let parsed = pre_parse(spec, arguments);
    let mut values = Vec::new();
    let mut errors = Vec::new();
    for param in &spec.params {
        match parsed.get(&param.name) {
            None if param.required => errors.push(LineError::new(
                &param.name,
                "missing",
                "Field required".into(),
                &Value::Object(parsed.clone()),
            )),
            None => values.push((param.name.clone(), default_arg(&param.default))),
            Some(value) => match check(param, value) {
                Ok(arg) => values.push((param.name.clone(), arg)),
                Err((kind, message)) => errors.push(LineError::new(&param.name, kind, message, value)),
            },
        }
    }
    if errors.is_empty() {
        Ok(values)
    } else {
        Err(render(&spec.model, &errors))
    }
}

fn default_arg(value: &Value) -> Arg {
    match value {
        Value::String(s) => Arg::Str(s.clone()),
        Value::Bool(b) => Arg::Bool(*b),
        Value::Number(n) => Arg::Int(n.to_string()),
        _ => Arg::Null,
    }
}

/// SDK 의 pre_parse_json: 꼴이 str 이 아닌 칸에 글이 오면 JSON 으로 풀어 본다. 푼 값이 str·int·float 면 그대로 둔다
/// (파이썬에서 bool 은 int 라 'true' 도 그대로 둔다). list·dict·null 만 바꾼다.
fn pre_parse(spec: &ToolSpec, arguments: &Map<String, Value>) -> Map<String, Value> {
    let mut out = arguments.clone();
    for (key, value) in arguments {
        let Some(param) = spec.params.iter().find(|p| &p.name == key) else {
            continue;
        };
        if param.plain_str() {
            continue;
        }
        if let Value::String(text) = value
            && let Ok(parsed) = serde_json::from_str::<Value>(text)
            && matches!(parsed, Value::Array(_) | Value::Object(_) | Value::Null)
        {
            out.insert(key.clone(), parsed);
        }
    }
    out
}

/// 검사 하나의 실패: (pydantic 오류 종류, 글)
pub type Fail = (&'static str, String);

/// i64 범위의 끝 (±2^63, f64 로 정확히 나타난다). pydantic 은 이 밖의 float 를 int 로 바꾸지 않는다 (int_parsing_size)
const I64_LOW: f64 = i64::MIN as f64;
const I64_HIGH: f64 = i64::MAX as f64;

fn check(param: &Param, value: &Value) -> Result<Arg, Fail> {
    if param.nullable && value.is_null() {
        return Ok(Arg::Null);
    }
    match &param.shape {
        Shape::Str { min, max } => check_str(value, *min, *max),
        Shape::Literal(options) => match value {
            Value::String(s) if options.contains(s) => Ok(Arg::Str(s.clone())),
            _ => Err(("literal_error", format!("Input should be {}", expected_literals(options)))),
        },
        Shape::Bool => check_bool(value),
        Shape::Int { min, max } => check_int(value, *min, *max),
    }
}

pub(crate) fn check_str(value: &Value, min: Option<usize>, max: Option<usize>) -> Result<Arg, Fail> {
    let Value::String(text) = value else {
        return Err(("string_type", "Input should be a valid string".into()));
    };
    let length = text.chars().count(); // pydantic 은 글자(코드 포인트) 수로 센다
    let plural = |n: usize| if n == 1 { "character" } else { "characters" };
    if let Some(min) = min
        && length < min
    {
        return Err(("string_too_short", format!("String should have at least {min} {}", plural(min))));
    }
    if let Some(max) = max
        && length > max
    {
        return Err(("string_too_long", format!("String should have at most {max} {}", plural(max))));
    }
    Ok(Arg::Str(text.clone()))
}

/// 'a', 'b' or 'c' (파이썬 repr 로)
fn expected_literals(options: &[String]) -> String {
    let quoted: Vec<String> = options.iter().map(|o| pyrepr::str_repr(o)).collect();
    match quoted.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
        None => String::new(),
    }
}

fn check_bool(value: &Value) -> Result<Arg, Fail> {
    let parsing = || {
        (
            "bool_parsing",
            "Input should be a valid boolean, unable to interpret input".to_string(),
        )
    };
    let wrong_type = || ("bool_type", "Input should be a valid boolean".to_string());
    match value {
        Value::Bool(b) => Ok(Arg::Bool(*b)),
        Value::Number(n) => {
            let text = n.to_string();
            // pydantic: 정수(i64 안)는 0·1 만, 그 밖 i64 안은 bool_parsing, i64 밖은 bool_type. float 는 소수 부분이 있으면 bool_type,
            // 정수 값이면 정수처럼 (2.0·1e18 → bool_parsing) — 정답 파일에서 확인 (2026-11-27)
            let whole = if is_float_text(&text) {
                match text.parse::<f64>() {
                    Ok(f) if f.fract() == 0.0 && f > I64_LOW && f < I64_HIGH => Some(f as i64),
                    _ => None,
                }
            } else {
                int_text(&text).parse::<i64>().ok()
            };
            match whole {
                Some(0) => Ok(Arg::Bool(false)),
                Some(1) => Ok(Arg::Bool(true)),
                Some(_) => Err(parsing()),
                None => Err(wrong_type()),
            }
        }
        Value::String(s) => match s.to_lowercase().as_str() {
            "0" | "off" | "f" | "false" | "n" | "no" => Ok(Arg::Bool(false)),
            "1" | "on" | "t" | "true" | "y" | "yes" => Ok(Arg::Bool(true)),
            _ => Err(parsing()),
        },
        _ => Err(wrong_type()),
    }
}

pub(crate) fn check_int(value: &Value, min: Option<i128>, max: Option<i128>) -> Result<Arg, Fail> {
    let parsing = || {
        (
            "int_parsing",
            "Input should be a valid integer, unable to parse string as an integer".to_string(),
        )
    };
    let digits = match value {
        Value::Bool(b) => (if *b { "1" } else { "0" }).to_string(),
        Value::Number(n) => {
            let text = n.to_string();
            if is_float_text(&text) {
                let f: f64 = text.parse().unwrap_or(f64::NAN);
                if f.fract() != 0.0 && f.is_finite() {
                    return Err((
                        "int_from_float",
                        "Input should be a valid integer, got a number with a fractional part".into(),
                    ));
                }
                if !(f > I64_LOW && f < I64_HIGH) {
                    return Err((
                        "int_parsing_size",
                        "Unable to parse input string as an integer, exceeded maximum size".into(),
                    ));
                }
                format!("{}", f as i64)
            } else {
                int_text(&text)
            }
        }
        Value::String(s) => int_from_str(s).ok_or_else(parsing)?,
        _ => return Err(("int_type", "Input should be a valid integer".into())),
    };
    if let Some(min) = min
        && compare_int(&digits, min) == std::cmp::Ordering::Less
    {
        return Err(("greater_than_equal", format!("Input should be greater than or equal to {min}")));
    }
    if let Some(max) = max
        && compare_int(&digits, max) == std::cmp::Ordering::Greater
    {
        return Err(("less_than_equal", format!("Input should be less than or equal to {max}")));
    }
    Ok(Arg::Int(digits))
}

/// pydantic 의 글 → int: 앞뒤 공백을 빼고, 부호 하나, 숫자 사이 밑줄 하나씩 허락, 끝의 '.0…' 은 허락 ('1.' 은 아님).
/// 아라비아 숫자(0-9)만. 크기 제한 없음 (파이썬 int).
fn int_from_str(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let (sign, rest) = match trimmed.as_bytes().first()? {
        b'-' => ("-", &trimmed[1..]),
        b'+' => ("", &trimmed[1..]),
        _ => ("", trimmed),
    };
    let whole = match rest.split_once('.') {
        Some((whole, zeros)) if !zeros.is_empty() && zeros.bytes().all(|b| b == b'0') => whole,
        Some(_) => return None,
        None => rest,
    };
    let bytes = whole.as_bytes();
    if bytes.is_empty() || !bytes[0].is_ascii_digit() || !bytes[bytes.len() - 1].is_ascii_digit() {
        return None;
    }
    let mut digits = String::new();
    for (n, b) in bytes.iter().enumerate() {
        match b {
            b'0'..=b'9' => digits.push(*b as char),
            b'_' if bytes[n + 1] != b'_' => {}
            _ => return None,
        }
    }
    let digits = digits.trim_start_matches('0');
    Some(if digits.is_empty() {
        "0".to_string()
    } else {
        format!("{sign}{digits}")
    })
}

fn compare_int(digits: &str, bound: i128) -> std::cmp::Ordering {
    match digits.parse::<i128>() {
        Ok(value) => value.cmp(&bound),
        Err(_) if digits.starts_with('-') => std::cmp::Ordering::Less,
        Err(_) => std::cmp::Ordering::Greater,
    }
}

pub(crate) struct LineError {
    loc: String,
    kind: &'static str,
    message: String,
    input_value: String,
    input_type: &'static str,
}

impl LineError {
    pub(crate) fn new(loc: &str, kind: &'static str, message: String, input: &Value) -> Self {
        Self {
            loc: loc.to_string(),
            kind,
            message,
            input_value: pyrepr::truncated(&pyrepr::repr(input)),
            input_type: type_name(input),
        }
    }
}

/// pydantic ValidationError 의 str 그대로. 판(2.14)은 자산 constants.json 의 pydantic_version.
pub(crate) fn render(model: &str, errors: &[LineError]) -> String {
    let count = errors.len();
    let mut out = format!("{count} validation error{} for {model}", if count == 1 { "" } else { "s" });
    for e in errors {
        out.push_str(&format!(
            "\n{}\n  {} [type={}, input_value={}, input_type={}]\n    For further information visit https://errors.pydantic.dev/{}/v/{}",
            e.loc,
            e.message,
            e.kind,
            e.input_value,
            e.input_type,
            crate::assets::get().pydantic_version,
            e.kind
        ));
    }
    out
}
