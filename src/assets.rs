//! 파이썬 서버에서 뽑은 자산 (assets/). 글을 Rust 에서 다시 쓰지 않는다.
//! 자산이 지금 파이썬 서버와 같은지는 파이썬 쪽 시험이 본다.

use std::sync::OnceLock;

use serde_json::Value;

use crate::validate::{ToolSpec, spec_from_schema};

const INITIALIZE: &str = include_str!("../assets/initialize.json");
const TOOLS: &str = include_str!("../assets/tools.json");
const SERVER_CARD: &str = include_str!("../assets/server_card.json");
const AI_CATALOG: &str = include_str!("../assets/ai_catalog.json");
const CONSTANTS: &str = include_str!("../assets/constants.json");
const MODERN: &str = include_str!("../assets/modern.json");
/// 여러 사용자가 한 IP 로 오는 범위 (파이썬 판과 같은 목록을 옮겨 둔 것)
const SHARED_RANGES: &str = include_str!("../assets/shared_ranges.txt");

pub struct Limits {
    pub per_minute: usize,
    pub per_day: usize,
    pub total_per_minute: usize,
    pub shared_per_minute: usize,
    pub group_factor: usize,
}

pub struct Assets {
    /// initialize 결과 (protocolVersion 은 요청마다 맞춘다)
    pub initialize: Value,
    /// tools/list 결과
    pub tools: Value,
    pub specs: Vec<ToolSpec>,
    pub card_body: String,
    pub card_etag: String,
    pub card_content_type: String,
    pub catalog_body: String,
    pub catalog_content_type: String,
    pub version: String,
    /// 처음 손잡기(initialize)로 받는 판들 (mcp_types HANDSHAKE_PROTOCOL_VERSIONS). 이 밖의 판 머리는 SDK 가 새 판(2026-07-28) 길로 보낸다
    pub handshake_versions: Vec<String>,
    pub pydantic_version: String,
    pub error_en: serde_json::Map<String, Value>,
    pub key_refused: serde_json::Map<String, Value>,
    pub fields_ttl_seconds: u64,
    pub upstream_retry_seconds: u64,
    pub limits: Limits,
    pub shared_ranges: Vec<ipnet::IpNet>,
    /// 2026-07-28 새 판들 (mcp_types MODERN_PROTOCOL_VERSIONS)
    pub modern_versions: Vec<String>,
    /// 새 판의 server/discover 결과 (파이썬 SDK 가 낸 그대로, _meta 도장 포함)
    pub discover: serde_json::Value,
    /// 새 판 결과마다 붙는 `_meta["io.modelcontextprotocol/serverInfo"]`
    pub server_info_stamp: serde_json::Value,
}

static ASSETS: OnceLock<Assets> = OnceLock::new();

pub fn get() -> &'static Assets {
    ASSETS.get_or_init(load)
}

fn parse(text: &str, name: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|e| panic!("자산 {name}: {e}"))
}

fn text(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_else(|| panic!("자산 칸 {key}")).to_string()
}

fn load() -> Assets {
    let initialize = parse(INITIALIZE, "initialize.json");
    let tools = parse(TOOLS, "tools.json");
    let card = parse(SERVER_CARD, "server_card.json");
    let catalog = parse(AI_CATALOG, "ai_catalog.json");
    let constants = parse(CONSTANTS, "constants.json");
    let specs = tools["tools"].as_array().expect("tools").iter().map(spec_from_schema).collect();
    let limits = &constants["limits"];
    let limit = |key: &str| limits[key].as_u64().unwrap_or_else(|| panic!("limits.{key}")) as usize;
    let handshake_versions = constants["handshake_protocol_versions"]
        .as_array()
        .expect("handshake_protocol_versions")
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    let shared_ranges = SHARED_RANGES
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.parse().unwrap_or_else(|e| panic!("shared_ranges {line}: {e}")))
        .collect();
    let modern = parse(MODERN, "modern.json");
    let discover = modern["discover"].clone();
    let server_info_stamp = discover["_meta"]["io.modelcontextprotocol/serverInfo"].clone();
    assert!(server_info_stamp.is_object(), "modern.json discover._meta 도장");
    let modern_versions = modern["versions"]
        .as_array()
        .expect("modern.versions")
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    Assets {
        modern_versions,
        discover,
        server_info_stamp,
        initialize,
        tools,
        specs,
        card_body: text(&card, "body"),
        card_etag: text(&card, "etag"),
        card_content_type: text(&card, "content_type"),
        catalog_body: text(&catalog, "body"),
        catalog_content_type: text(&catalog, "content_type"),
        version: text(&constants, "version"),
        handshake_versions,
        pydantic_version: text(&constants, "pydantic_version"),
        error_en: constants["error_en"].as_object().expect("error_en").clone(),
        key_refused: constants["key_refused"].as_object().expect("key_refused").clone(),
        fields_ttl_seconds: constants["fields_ttl_seconds"].as_u64().expect("fields_ttl_seconds"),
        upstream_retry_seconds: constants["upstream_retry_seconds"].as_u64().expect("upstream_retry_seconds"),
        limits: Limits {
            per_minute: limit("per_minute"),
            per_day: limit("per_day"),
            total_per_minute: limit("total_per_minute"),
            shared_per_minute: limit("shared_per_minute"),
            group_factor: limit("group_factor"),
        },
        shared_ranges,
    }
}

impl Assets {
    /// 계정 키를 쓸 수 없는 이유 글 (파이썬 server.REFUSED, 모르는 code 는 기본 글)
    pub fn key_refused(&self, code: &str) -> String {
        self.key_refused
            .get(code)
            .and_then(Value::as_str)
            .unwrap_or("이 키를 쓸 수 없습니다. / This key cannot be used.")
            .to_string()
    }

    pub fn error_en(&self, code: Option<&str>) -> String {
        code.and_then(|c| self.error_en.get(c))
            .and_then(Value::as_str)
            .unwrap_or("Request failed")
            .to_string()
    }

    /// 마지막 손잡기 판 (요청한 판이 목록에 없을 때 initialize 가 내는 판, SDK LATEST_HANDSHAKE_VERSION)
    pub fn latest_handshake(&self) -> &str {
        self.handshake_versions.last().expect("손잡기 판")
    }
}
