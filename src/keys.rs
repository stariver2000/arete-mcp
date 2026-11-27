//! 원격 MCP 계정 키 — 파이썬 판과 같은 규칙.
//!
//! 키(ajl_…)는 `Authorization: Bearer` 를 먼저, 없으면 `?key=`. 마켓 API 의 안쪽 경로(POST /v1/internal/mcp-keys/check, 내부 키)에 본문으로 보내
//! 확인하고, 결과를 키 해시마다 60초 담는다. 200 = 키 한도로 센다, 401 = 익명으로 돈다, 403 = 이유를 알린다, 그 밖(확인 서버가 못 답함) = 이번만 익명.
//! 한도: 키마다 분당 per_minute, 회원(quota_key)마다 하루 daily_limit — 하루 셈은 확인 때 받은 used_today 에서 이어 센다.
//! 쓴 양: (토큰, 한국 날짜)마다 모아 1분마다 POST /v1/internal/mcp-keys/usage, 못 보내면 다음에 다시.
//! 키 원문은 확인 요청 한 번에만 쓰고, 담는 열쇠는 sha256 이다. 기록·오류 글 어디에도 남기지 않는다.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::capped::Capped;
use crate::{assets, limits};

const KEY_PREFIX: &str = "ajl_";
const CHECK_TTL: Duration = Duration::from_secs(60);
pub const FLUSH_EVERY: Duration = Duration::from_secs(60);
const CACHE_MAX: usize = 10_000;
/// 담기에 없는 키를 확인하러 가는 수 (파이썬 keys.CHECKS_*): 기계(host_key, IPv6 /64)마다 분당 10, 망(/24·/48)은 그 3배,
/// Claude 커넥터 범위는 제 몫 분당 120, 서버 전체 분당 300 (보안 점검 2026-11-27 #3, 재점검 N2)
const CHECKS_PER_MINUTE: usize = 10;
const CHECKS_GROUP_FACTOR: usize = 3;
const CHECKS_SHARED_PER_MINUTE: usize = 120;
const CHECKS_TOTAL_PER_MINUTE: usize = 300;
const KST_OFFSET_SECONDS: i64 = 9 * 3600;

#[derive(Clone, Debug, PartialEq)]
pub struct KeyInfo {
    pub token_id: i64,
    pub quota_key: String,
    pub used_today: i64,
    pub daily_limit: i64,
    pub per_minute: i64,
}

/// 확인 결과 (담아 두는 값)
#[derive(Clone, Debug)]
enum Checked {
    Key(KeyInfo),
    /// 맞는 키인데 쓸 수 없다: 403 의 code (권한 없음·정지·14세 미확인)
    Refused(String),
}

pub enum KeyOutcome {
    Key(KeyInfo),
    Anonymous,
    Refused(String),
}

/// 파이썬 key_from: Bearer 를 먼저, 없으면 ?key= (starlette QueryParams.get = 같은 이름의 마지막 값). 우리 모양(ajl_)이 아니면 없음.
pub fn key_from(authorization: Option<&str>, query: Option<&str>) -> Option<String> {
    if let Some(auth) = authorization
        && auth.len() >= 7
        && auth[..7].eq_ignore_ascii_case("bearer ")
    {
        let value = auth[7..].trim_matches(crate::text::is_py_space);
        if value.starts_with(KEY_PREFIX) {
            return Some(value.to_string());
        }
    }
    let value = query?
        .split('&')
        .filter_map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            (form_decode(name) == "key").then(|| form_decode(value))
        })
        .next_back()?;
    let value = value.trim_matches(crate::text::is_py_space).to_string();
    value.starts_with(KEY_PREFIX).then_some(value)
}

/// application/x-www-form-urlencoded 풀기 (+ 는 빈칸, %XX). 틀린 % 는 그대로 둔다 (파이썬 unquote_plus 와 같다)
fn form_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut n = 0;
    while n < bytes.len() {
        match bytes[n] {
            b'+' => out.push(b' '),
            b'%' if n + 2 < bytes.len() => match std::str::from_utf8(&bytes[n + 1..n + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                Some(b) => {
                    out.push(b);
                    n += 2;
                }
                None => out.push(b'%'),
            },
            b => out.push(b),
        }
        n += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn digest(key: &str) -> String {
    Sha256::digest(key.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// 확인 결과를 60초 담아 둔다 (파이썬 KeyBook). 모르는 키(401)는 따로 담아, 아무 값이나 쏟아도 맞는 키(200·403)를 밀어내지 못한다.
/// 담기에 없는 키를 확인하러 가는 수는 IP 마다 분당 CHECKS_PER_MINUTE (보안 점검 2026-11-27 #3, 파이썬 keys.py 와 같은 값).
#[derive(Default)]
pub struct KeyBook {
    state: Mutex<BookState>,
}

struct BookState {
    known: Capped<(Instant, Checked)>,
    unknown: Capped<Instant>,
    /// "h:<host_key>"·"n:<network_key>" → 지난 60초의 확인 때
    checks: Capped<VecDeque<Instant>>,
    /// 표 밖이라 밀려나지 않는다
    checks_shared: VecDeque<Instant>,
    checks_total: VecDeque<Instant>,
}

impl Default for BookState {
    fn default() -> Self {
        Self {
            known: Capped::new(CACHE_MAX),
            unknown: Capped::new(CACHE_MAX),
            checks: Capped::new(CACHE_MAX),
            checks_shared: VecDeque::new(),
            checks_total: VecDeque::new(),
        }
    }
}

impl BookState {
    /// 지난 60초의 확인 수가 칸마다 몫 아래면 하나씩 쓰고 true: 기계·망 (Claude 범위면 그 몫만)·서버 전체 (파이썬 _spend_check).
    /// 칸은 쓴 때에만 표에 넣는다
    fn spend_check(&mut self, who: &str, now: Instant) -> bool {
        let expired = |at: &Instant| now.duration_since(*at) >= Duration::from_secs(60);
        let trim = |spent: &mut VecDeque<Instant>| {
            while spent.front().is_some_and(expired) {
                spent.pop_front();
            }
        };
        trim(&mut self.checks_total);
        let named: Vec<(String, usize)> = if limits::in_ranges(who, &assets::get().shared_ranges) {
            trim(&mut self.checks_shared);
            if self.checks_shared.len() >= CHECKS_SHARED_PER_MINUTE {
                return false;
            }
            vec![]
        } else {
            vec![
                (format!("h:{}", limits::host_key(who)), CHECKS_PER_MINUTE),
                (format!("n:{}", limits::network_key(who)), CHECKS_PER_MINUTE * CHECKS_GROUP_FACTOR),
            ]
        };
        for (name, limit) in &named {
            if let Some(spent) = self.checks.map.get_mut(name) {
                trim(spent);
                if spent.len() >= *limit {
                    return false;
                }
            }
        }
        if self.checks_total.len() >= CHECKS_TOTAL_PER_MINUTE {
            return false;
        }
        if named.is_empty() {
            self.checks_shared.push_back(now);
        }
        for (name, _) in named {
            if !self.checks.map.contains_key(&name) {
                self.checks.put(name.clone(), VecDeque::new());
            }
            if let Some(spent) = self.checks.map.get_mut(&name) {
                spent.push_back(now);
            }
        }
        self.checks_total.push_back(now);
        true
    }
}

impl KeyBook {
    /// who: 요청한 사용자 IP. remote(key) → (HTTP 상태, 본문). 0 은 닿지 않음
    pub async fn check<F, Fut>(&self, key: &str, who: &str, remote: F) -> KeyOutcome
    where
        F: FnOnce(String) -> Fut,
        Fut: std::future::Future<Output = (u16, Value)>,
    {
        let digest = digest(key);
        let now = Instant::now();
        let fresh = {
            let mut state = self.state.lock().expect("키 잠금");
            let fresh = state
                .known
                .map
                .get(&digest)
                .filter(|(at, _)| at.elapsed() < CHECK_TTL)
                .map(|(_, c)| c.clone());
            if fresh.is_none() {
                if state.unknown.map.get(&digest).is_some_and(|at| at.elapsed() < CHECK_TTL) {
                    return KeyOutcome::Anonymous;
                }
                if !state.spend_check(who, now) {
                    return KeyOutcome::Anonymous; // 확인 몫을 다 썼다: 이번만 익명 (담지 않는다)
                }
            }
            fresh
        };
        let checked = match fresh {
            Some(checked) => checked,
            None => {
                let (status, body) = remote(key.to_string()).await;
                let checked = match status {
                    200 => match key_info(&body) {
                        Some(info) => Checked::Key(info),
                        None => return KeyOutcome::Anonymous, // 파이썬: 칸이 모자라면 예외 → 이 경우는 오지 않는다고 보고 익명
                    },
                    403 => Checked::Refused(
                        body.get("code")
                            .and_then(Value::as_str)
                            .filter(|c| !c.is_empty())
                            .unwrap_or("forbidden")
                            .to_string(),
                    ),
                    401 => {
                        let mut state = self.state.lock().expect("키 잠금");
                        state.known.remove(&digest);
                        state.unknown.put(digest, now);
                        return KeyOutcome::Anonymous;
                    }
                    _ => return KeyOutcome::Anonymous, // 확인 서버가 답하지 못하면 이번만 익명 (담지 않는다)
                };
                let mut state = self.state.lock().expect("키 잠금");
                state.unknown.remove(&digest);
                state.known.put(digest, (now, checked.clone()));
                checked
            }
        };
        match checked {
            Checked::Key(info) => KeyOutcome::Key(info),
            Checked::Refused(code) => KeyOutcome::Refused(code),
        }
    }
}

fn key_info(body: &Value) -> Option<KeyInfo> {
    let int = |name: &str| body.get(name).and_then(Value::as_i64);
    Some(KeyInfo {
        token_id: int("token_id")?,
        quota_key: body.get("quota_key").and_then(Value::as_str)?.to_string(),
        used_today: int("used_today")?,
        daily_limit: int("daily_limit")?,
        per_minute: int("per_minute")?,
    })
}

/// 지금 한국 날짜 (1970-01-01 부터의 날 수) 와 다음 자정까지 남은 초
fn kst_day_and_left() -> (i64, i64) {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).expect("시계").as_secs_f64() + KST_OFFSET_SECONDS as f64;
    let day = (now / 86400.0).floor() as i64;
    let left = ((day + 1) as f64 * 86400.0 - now) as i64; // 파이썬 int(): 0 쪽으로 자른다
    (day, left)
}

/// 키마다 분당, 회원마다 하루 (파이썬 KeyLimiter). 막힌 요청은 세지 않는다.
#[derive(Default)]
pub struct KeyLimiter {
    started: Option<Instant>,
    minute: HashMap<i64, VecDeque<f64>>,
    day: HashMap<String, (i64, i64)>,
    checks: u64,
}

/// 키 한도가 이만큼 셀 때마다 지난 칸을 치운다 (파이썬 keys.PURGE_EVERY, 보안 점검 2026-11-27 #8)
const PURGE_EVERY: u64 = 1000;

impl KeyLimiter {
    pub fn check(&mut self, info: &KeyInfo) -> Option<u64> {
        let started = *self.started.get_or_insert_with(Instant::now);
        let now = started.elapsed().as_secs_f64();
        let (today, left) = kst_day_and_left();
        self.checks += 1;
        if self.checks.is_multiple_of(PURGE_EVERY) {
            // 1분 넘게 안 쓴 키, 오늘이 아닌 회원
            self.minute.retain(|_, hits| hits.back().is_some_and(|last| *last >= now - 60.0));
            self.day.retain(|_, seen| seen.0 == today);
        }
        let minute = self.minute.entry(info.token_id).or_default();
        while minute.front().is_some_and(|first| *first < now - 60.0) {
            minute.pop_front();
        }
        let (seen_day, mut count) = self.day.get(&info.quota_key).copied().unwrap_or((today, info.used_today));
        if seen_day != today {
            count = 0;
        }
        count = count.max(if seen_day == today { info.used_today } else { 0 });
        if info.per_minute > 0 && minute.len() as i64 >= info.per_minute {
            return Some(1.max((minute[0] + 60.0 - now) as i64 + 1) as u64);
        }
        if info.daily_limit > 0 && count >= info.daily_limit {
            return Some(1.max(left) as u64);
        }
        minute.push_back(now);
        self.day.insert(info.quota_key.clone(), (today, count + 1));
        None
    }
}

/// (토큰, 한국 날)마다 호출 수 (파이썬 UsageBuffer)
#[derive(Default)]
pub struct UsageBuffer {
    counts: Mutex<HashMap<(i64, i64), i64>>,
}

impl UsageBuffer {
    pub fn add(&self, token_id: i64) {
        let (day, _) = kst_day_and_left();
        *self.counts.lock().expect("쓴 양 잠금").entry((token_id, day)).or_default() += 1;
    }

    /// 보낼 줄을 꺼낸다 ({token_id, day: "YYYY-MM-DD", calls})
    pub fn take(&self) -> Vec<((i64, i64), i64)> {
        self.counts.lock().expect("쓴 양 잠금").drain().collect()
    }

    /// 보내지 못한 줄을 다시 넣는다 (그 사이 쌓인 것과 더해). 어제보다 오랜 날은 버린다: API 가 계속 못 받아도 쌓이지 않게 (파이썬 UsageBuffer.flush, #8)
    pub fn put_back(&self, rows: Vec<((i64, i64), i64)>) {
        let (today, _) = kst_day_and_left();
        let mut counts = self.counts.lock().expect("쓴 양 잠금");
        for (key, calls) in rows.into_iter().filter(|((_, day), _)| *day >= today - 1) {
            *counts.entry(key).or_default() += calls;
        }
    }

    pub fn rows_json(rows: &[((i64, i64), i64)]) -> Value {
        Value::Array(
            rows.iter()
                .map(|((token_id, day), calls)| json!({"token_id": token_id, "day": iso_day(*day), "calls": calls}))
                .collect(),
        )
    }
}

/// 1970-01-01 부터의 날 수 → YYYY-MM-DD (그레고리력, Howard Hinnant 의 civil_from_days)
fn iso_day(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_from_like_python() {
        assert_eq!(key_from(Some("Bearer ajl_abc"), None).as_deref(), Some("ajl_abc"));
        assert_eq!(key_from(Some("bearer  ajl_abc "), Some("key=ajl_q")).as_deref(), Some("ajl_abc"));
        assert_eq!(key_from(Some("Bearer other"), Some("key=ajl_q")).as_deref(), Some("ajl_q"));
        assert_eq!(key_from(None, Some("a=1&key=ajl_one&key=ajl_two")).as_deref(), Some("ajl_two"));
        assert_eq!(key_from(None, Some("key=%61jl_x")).as_deref(), Some("ajl_x"));
        assert_eq!(key_from(None, Some("key=nope")), None);
        assert_eq!(key_from(None, None), None);
    }

    fn run<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("tokio")
            .block_on(future)
    }

    #[test]
    fn check_outcomes_and_caching_like_python() {
        // 확인 응답 → 결과: 200 키, 401(모르는·폐기·만료 키) 익명, 403 이유, 닿지 않음(0)·500 이번만 익명. 401·403·200 은 60초 담고 0·500 은 담지 않는다
        let book = KeyBook::default();
        let calls = std::cell::Cell::new(0);
        let remote = |status: u16, body: Value| {
            calls.set(calls.get() + 1);
            async move { (status, body) }
        };
        let ok = json!({"token_id": 4, "quota_key": "mq", "used_today": 7, "daily_limit": 5000, "per_minute": 120});
        assert!(matches!(run(book.check("ajl_live", IP, |_| remote(200, ok.clone()))), KeyOutcome::Key(info) if info.used_today == 7));
        assert!(matches!(
            run(book.check("ajl_revoked", IP, |_| remote(401, json!({"code": "token_invalid"})))),
            KeyOutcome::Anonymous
        ));
        assert!(
            matches!(run(book.check("ajl_scope", IP, |_| remote(403, json!({"code": "scope_missing"})))), KeyOutcome::Refused(code) if code == "scope_missing")
        );
        assert!(matches!(
            run(book.check("ajl_down", IP, |_| remote(0, json!({})))),
            KeyOutcome::Anonymous
        ));
        assert_eq!(calls.get(), 4);
        // 담긴 셋은 다시 묻지 않는다, 닿지 않았던 것은 다시 묻는다
        run(book.check("ajl_revoked", IP, |_| remote(200, ok.clone())));
        run(book.check("ajl_scope", IP, |_| remote(200, ok.clone())));
        run(book.check("ajl_down", IP, |_| remote(401, json!({}))));
        assert_eq!(calls.get(), 5);
    }

    const IP: &str = "1.2.3.4";

    #[test]
    fn key_checks_are_capped_per_ip_and_unknown_keys_cannot_push_good_keys_out() {
        // 파이썬 test_key_checks_are_capped_per_ip_and_unknown_keys_cannot_push_good_keys_out 와 같은 이야기 (시계를 못 돌려 1분 뒤 칸만 뺀다)
        let book = KeyBook::default();
        let calls = std::cell::Cell::new(0);
        let remote = |status: u16, body: Value| {
            calls.set(calls.get() + 1);
            async move { (status, body) }
        };
        let ok = json!({"token_id": 7, "quota_key": "mq", "used_today": 0, "daily_limit": 5, "per_minute": 3});
        assert!(matches!(
            run(book.check("ajl_good", "9.9.9.9", |_| remote(200, ok.clone()))),
            KeyOutcome::Key(_)
        ));
        for n in 0..CHECKS_PER_MINUTE + 5 {
            assert!(matches!(
                run(book.check(&format!("ajl_guess{n}"), IP, |_| remote(401, json!({})))),
                KeyOutcome::Anonymous
            ));
        }
        assert_eq!(calls.get(), 1 + CHECKS_PER_MINUTE); // 넘친 다섯은 확인하러 가지 않았다
        run(book.check("ajl_other", "5.5.5.5", |_| remote(401, json!({}))));
        assert_eq!(calls.get(), 2 + CHECKS_PER_MINUTE); // 다른 IP 는 제 몫
        run(book.check("ajl_guess0", IP, |_| remote(200, ok.clone())));
        assert_eq!(calls.get(), 2 + CHECKS_PER_MINUTE); // 모르는 키도 60초 담는다
        // 모르는 키 담기를 넘치게 채워도 (확인 몫은 위에서 봤으니 담기에 바로 넣는다 — 파이썬 시험도 몫을 건너뛴다)
        for n in 0..CACHE_MAX + 50 {
            book.state.lock().unwrap().unknown.put(format!("flood{n}"), Instant::now());
        }
        assert_eq!(book.state.lock().unwrap().unknown.map.len(), CACHE_MAX);
        let before = calls.get();
        assert!(matches!(
            run(book.check("ajl_good", "9.9.9.9", |_| remote(500, json!({})))),
            KeyOutcome::Key(_)
        ));
        assert_eq!(calls.get(), before); // 맞는 키는 그대로 담겨 있다
    }

    #[test]
    fn days_to_iso() {
        assert_eq!(iso_day(0), "1970-01-01");
        assert_eq!(iso_day(20_784), "2026-11-27");
        assert_eq!(iso_day(11_016), "2000-02-29");
    }

    #[test]
    fn key_limits_per_minute_and_day() {
        let mut limiter = KeyLimiter::default();
        let info = KeyInfo {
            token_id: 1,
            quota_key: "q".into(),
            used_today: 0,
            daily_limit: 5,
            per_minute: 3,
        };
        for _ in 0..3 {
            assert_eq!(limiter.check(&info), None);
        }
        assert!(limiter.check(&info).is_some_and(|w| (1..=61).contains(&w)));
        let near_daily = KeyInfo {
            token_id: 2,
            quota_key: "q2".into(),
            used_today: 5,
            daily_limit: 5,
            per_minute: 100,
        };
        assert!(limiter.check(&near_daily).is_some_and(|w| w >= 1));
    }

    #[test]
    fn usage_rows_older_than_yesterday_are_dropped() {
        // 파이썬 test_key_limiter_and_usage_drop_past_entries 의 쓴 양 쪽 (#8)
        let buffer = UsageBuffer::default();
        let (today, _) = kst_day_and_left();
        buffer.put_back(vec![((7, today - 2), 3), ((7, today - 1), 2), ((7, today), 1)]);
        let mut rows = buffer.take();
        rows.sort();
        assert_eq!(rows, vec![((7, today - 1), 2), ((7, today), 1)]);
    }

    #[test]
    fn key_check_budget_is_per_64_and_network_with_a_claude_share_and_a_server_total() {
        // 파이썬 test_key_check_budget_is_per_64_and_network_with_a_claude_share_and_a_server_total (N2)
        let book = KeyBook::default();
        let calls = std::cell::Cell::new(0);
        let remote = |status: u16| {
            calls.set(calls.get() + 1);
            async move { (status, json!({})) }
        };
        let count = |keys: Vec<(String, String)>| {
            let before = calls.get();
            for (key, who) in keys {
                run(book.check(&key, &who, |_| remote(401)));
            }
            calls.get() - before
        };
        assert_eq!(
            count(
                (0..15)
                    .map(|n| (format!("ajl_a{n}"), format!("2001:db8:1:1::{:x}", n + 1)))
                    .collect()
            ),
            CHECKS_PER_MINUTE
        );
        assert_eq!(
            count(
                (0..40)
                    .map(|n| (format!("ajl_b{n}"), format!("2001:db8:1:{:x}::1", n + 2)))
                    .collect()
            ),
            CHECKS_PER_MINUTE * CHECKS_GROUP_FACTOR - CHECKS_PER_MINUTE
        );
        assert_eq!(
            count((0..130).map(|n| (format!("ajl_c{n}"), "160.79.105.7".to_string())).collect()),
            CHECKS_SHARED_PER_MINUTE
        );
        let used = CHECKS_PER_MINUTE * CHECKS_GROUP_FACTOR + CHECKS_SHARED_PER_MINUTE;
        assert_eq!(
            count(
                (0..400)
                    .map(|n| (format!("ajl_d{n}"), format!("10.{}.{}.1", n / 200, n % 200)))
                    .collect()
            ),
            CHECKS_TOTAL_PER_MINUTE - used
        );
    }
}
