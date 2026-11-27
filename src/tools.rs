//! 도구 여섯의 몸통 — 파이썬 판의 search·find_by_field·get_ranking·get_trending·get_item·list_fields 와
//! _guard·_resolve_field·_match_field·_note_empty·_fields 를 그대로 옮김. 글은 파이썬 쪽과 한 글자까지 같아야 한다.
//!
//! 계정 키 길(`?key=`·Bearer)은 keys.rs.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use crate::assets;
use crate::keys::{KeyBook, KeyLimiter, KeyOutcome, UsageBuffer};
use crate::limits::{Limiter, client_ip};
use crate::text;
use crate::upstream::{ToolFail, Upstream};
use crate::validate::{self, Arg};

/// 분야 목록 담기: kind(없으면 None) → (담은 때, 목록)
type FieldsCache = HashMap<Option<String>, (Instant, Vec<Value>)>;

/// 부른 쪽: 역방향 프록시가 넣은 X-Forwarded-For 와 계정 키(있으면). 키 원문은 확인 요청에만 쓴다 (keys.rs)
#[derive(Default)]
pub struct Caller {
    pub forwarded_for: Option<String>,
    pub key: Option<String>,
}

pub struct Tools {
    pub upstream: Upstream,
    limiter: Mutex<Limiter>,
    keys: KeyBook,
    key_limiter: Mutex<KeyLimiter>,
    pub usage: UsageBuffer,
    /// 분야 목록 10분 담기 (파이썬 _fields_cache, 열쇠 = kind 또는 None)
    fields_cache: Mutex<FieldsCache>,
}

/// tools/call 의 결과: 성공이면 공개 API 본문, 아니면 결과 글 (isError).
pub enum Outcome {
    Ok(Map<String, Value>),
    Error(String),
}

struct Args(Vec<(String, Arg)>);

impl Args {
    fn get(&self, name: &str) -> &Arg {
        &self.0.iter().find(|(n, _)| n == name).expect("스키마에 있는 칸").1
    }
    fn str(&self, name: &str) -> Option<String> {
        self.get(name).as_str().map(str::to_string)
    }
    /// 파이썬 `x or None` (bool 칸): true 일 때만 보낸다
    fn flag(&self, name: &str) -> Option<String> {
        self.get(name).as_bool().then(|| "true".to_string())
    }
    /// 파이썬 min(max(limit, 1), top)
    fn limit(&self, top: i128) -> Option<String> {
        let value: i128 = self.get("limit").as_int().parse().unwrap_or(top);
        Some(value.clamp(1, top).to_string())
    }
    /// 파이썬 _lang: 기본(ko)은 보내지 않는다
    fn lang(&self) -> Option<String> {
        (self.get("lang").as_str() == Some("en")).then(|| "en".to_string())
    }
}

impl Tools {
    pub fn new(upstream: Upstream) -> Self {
        Self {
            upstream,
            limiter: Mutex::new(Limiter::new()),
            keys: KeyBook::default(),
            key_limiter: Mutex::new(KeyLimiter::default()),
            usage: UsageBuffer::default(),
            fields_cache: Mutex::new(HashMap::new()),
        }
    }

    pub async fn call(&self, name: &str, arguments: &Map<String, Value>, caller: &Caller) -> Outcome {
        let a = assets::get();
        let Some(spec) = a.specs.iter().find(|s| s.name == name) else {
            return Outcome::Error(format!("Unknown tool: {name}")); // SDK tool_manager: 앞머리 없이
        };
        let args = match validate::validate(spec, arguments) {
            Ok(values) => Args(values),
            Err(message) => return Outcome::Error(format!("Error executing tool {name}: {message}")),
        };
        let result = match self.guard(caller).await {
            Err(fail) => Err(fail),
            Ok(()) => self.run(name, &args).await,
        };
        match result {
            Ok(body) => Outcome::Ok(body),
            Err(ToolFail::Arete(message)) => Outcome::Error(format!("Error executing tool {name}: {message}")),
            Err(ToolFail::Unexpected) => Outcome::Error(format!("Error executing tool {name}")),
        }
    }

    /// 파이썬 _guard: 계정 키가 맞으면 키마다 분당·회원마다 하루 (keys.rs), 아니면 들어온 사용자 IP 마다 (머리가 없으면 'direct' 한 칸)
    async fn guard(&self, caller: &Caller) -> Result<(), ToolFail> {
        let a = assets::get();
        let ip = client_ip(caller.forwarded_for.as_deref()).unwrap_or_else(|| "direct".to_string());
        if let Some(key) = &caller.key {
            match self.keys.check(key, &ip, |key| self.upstream.check_key(key)).await {
                KeyOutcome::Refused(code) => {
                    let text = a.key_refused(&code);
                    return Err(ToolFail::Arete(text));
                }
                KeyOutcome::Key(info) => {
                    let wait = self.key_limiter.lock().expect("키 한도 잠금").check(&info);
                    if let Some(wait) = wait {
                        return Err(ToolFail::Arete(format!(
                            "키 한도를 넘었습니다. {wait}초 뒤에 다시 시도하세요. / Key rate limit reached; retry in {wait}s."
                        )));
                    }
                    self.usage.add(info.token_id);
                    return Ok(());
                }
                KeyOutcome::Anonymous => {} // 틀린·폐기된 키·확인 서버가 못 답함: 익명으로 센다
            }
        }
        let wait = self.limiter.lock().expect("한도 잠금").check(&ip, &a.limits, &a.shared_ranges);
        match wait {
            None => Ok(()),
            Some(wait) => Err(ToolFail::Arete(format!(
                "요청 한도를 넘었습니다. {wait}초 뒤에 다시 시도하세요. / Rate limit reached; retry in {wait}s."
            ))),
        }
    }

    async fn run(&self, name: &str, args: &Args) -> Result<Map<String, Value>, ToolFail> {
        let up = &self.upstream;
        match name {
            "search" => {
                let text = text::take_chars(&text::squeeze_spaces(&args.str("query").unwrap_or_default()), 100);
                if text.is_empty() {
                    return Err(ToolFail::Arete(
                        "하고 싶은 일을 글로 넣어 주세요. / Describe the task in words.".into(),
                    ));
                }
                up.get(
                    "/items",
                    &[
                        ("q", Some(text)),
                        ("kind", args.str("kind")),
                        ("ko", args.flag("ko")),
                        ("limit", args.limit(10)),
                        ("lang", args.lang()),
                    ],
                )
                .await
            }
            "find_by_field" => {
                let kind = args.str("kind");
                let (key, has_items) = self.resolve_field(&args.str("field").unwrap_or_default(), kind.as_deref()).await?;
                let result = up
                    .get(
                        "/items",
                        &[
                            ("kind", kind.clone()),
                            ("field", Some(key)),
                            ("sort", Some("top".into())),
                            ("ko", args.flag("ko")),
                            ("free", args.flag("free")),
                            ("limit", args.limit(10)),
                            ("lang", args.lang()),
                        ],
                    )
                    .await?;
                Ok(note_empty(result, kind.as_deref().unwrap_or_default(), has_items))
            }
            "get_ranking" | "get_trending" => {
                let kind = args.str("kind");
                let field = args.str("field").filter(|f| !f.is_empty()); // 파이썬 `if field` (빈 글은 없음과 같다)
                let (key, has_items) = match field {
                    Some(field) => {
                        let (key, has) = self.resolve_field(&field, kind.as_deref()).await?;
                        (Some(key), has)
                    }
                    None => (None, true),
                };
                let (sort, top) = if name == "get_ranking" { ("top", 20) } else { ("trending", 10) };
                let result = up
                    .get(
                        "/items",
                        &[
                            ("kind", kind.clone()),
                            ("field", key),
                            ("sort", Some(sort.into())),
                            ("period", args.str("period")),
                            ("limit", args.limit(top)),
                        ],
                    )
                    .await?;
                Ok(note_empty(result, kind.as_deref().unwrap_or_default(), has_items))
            }
            "get_item" => {
                up.get(&format!("/items/{}", args.get("item_id").as_int()), &[("lang", args.lang())])
                    .await
            }
            "list_fields" => up.get("/fields", &[("kind", args.str("kind")), ("lang", args.lang())]).await,
            _ => Err(ToolFail::Unexpected),
        }
    }

    async fn fields(&self, kind: Option<&str>) -> Result<Vec<Value>, ToolFail> {
        let ttl = Duration::from_secs(assets::get().fields_ttl_seconds);
        let cache_key = kind.map(str::to_string);
        if let Some((at, listing)) = self.fields_cache.lock().expect("분야 잠금").get(&cache_key)
            && at.elapsed() < ttl
        {
            return Ok(listing.clone());
        }
        let body = self.upstream.get("/fields", &[("kind", cache_key.clone())]).await?;
        let listing = match body.get("fields") {
            Some(Value::Array(listing)) => listing.clone(),
            _ => return Err(ToolFail::Unexpected), // 파이썬 KeyError·형 오류
        };
        self.fields_cache
            .lock()
            .expect("분야 잠금")
            .insert(cache_key, (Instant::now(), listing.clone()));
        Ok(listing)
    }

    /// 파이썬 _resolve_field: (분야 키, 그 종류에 항목이 있나). 어느 목록에도 없으면 맨 위 분야를 다 알려 준다.
    async fn resolve_field(&self, field: &str, kind: Option<&str>) -> Result<(String, bool), ToolFail> {
        let listing = self.fields(kind).await?;
        if let Some(key) = match_field(field, &listing)? {
            return Ok((key, true));
        }
        if kind.is_some()
            && let Some(key) = match_field(field, &self.fields(None).await?)?
        {
            return Ok((key, false));
        }
        let tops: Vec<String> = listing
            .iter()
            .filter(|e| !e["key"].as_str().unwrap_or_default().contains('.'))
            .map(|e| format!("{}({})", py_text(&e["key"]), py_text(&e["label"])))
            .collect();
        Err(ToolFail::Arete(format!(
            "분야 '{field}' 를 찾지 못했습니다. 맨 위 분야: {}. 세부 분야는 list_fields 로 봅니다. / Unknown field '{field}'. Top-level fields are listed above; call list_fields for all of them.",
            tops.join(", ")
        )))
    }
}

/// 파이썬 _match_field: 키·이름이 같으면 그 키, 아니면 띄어쓰기·기호를 뺀 이름이 서로 들어 있는 것 중 짧은 이름 먼저.
fn match_field(field: &str, listing: &[Value]) -> Result<Option<String>, ToolFail> {
    let wanted = field.trim_matches(text::is_py_space);
    for entry in listing {
        if entry["key"].as_str() == Some(wanted) || entry["label"].as_str() == Some(wanted) {
            return Ok(entry["key"].as_str().map(str::to_string));
        }
    }
    let squashed = text::squash(wanted);
    if squashed.chars().count() < 2 {
        return Ok(None);
    }
    let mut near: Vec<(usize, String)> = Vec::new();
    for entry in listing {
        let Some(label) = entry["label"].as_str() else {
            return Err(ToolFail::Unexpected);
        }; // 파이썬 None.casefold → AttributeError
        let label_squashed = text::squash(label);
        if label_squashed.contains(&squashed) || squashed.contains(&label_squashed) {
            near.push((label.chars().count(), entry["key"].as_str().unwrap_or_default().to_string()));
        }
    }
    Ok(near.into_iter().min().map(|(_, key)| key))
}

/// 파이썬 _note_empty: 있는 분야인데 그 종류의 항목이 아직 없을 때 안내 한 줄을 끝에 붙인다.
fn note_empty(mut result: Map<String, Value>, kind: &str, has_items: bool) -> Map<String, Value> {
    if !has_items {
        result.insert(
            "note".into(),
            Value::String(format!(
                "이 분야에 이 종류({kind}) 항목이 아직 없어요. 다른 kind 로 찾거나 list_fields 의 kind_counts 를 보세요. / No items of this kind ({kind}) in this field yet; try another kind or check kind_counts in list_fields."
            )),
        );
    }
    result
}

/// f-string 안의 값 (글이면 그대로, 아니면 파이썬 str)
fn py_text(value: &Value) -> String {
    value.as_str().map_or_else(|| crate::pyrepr::repr(value), str::to_string)
}
