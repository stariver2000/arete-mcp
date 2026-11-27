//! 원격 MCP 의 IP 한도 — 파이썬 판과 같은 규칙 (값은 자산 constants.json 의 limits).
//!
//! 들어온 사용자 IP 마다 분당 30·하루 300, 같은 망(IPv4 /24·IPv6 /48)은 그 3배, 그 밖 사용자 전체 분당 300 — 전체 상한은 지난 1분에
//! IP 한도만큼 이미 쓴 망만 막는다 (몇 망이 상한을 채워도 조금 쓰는 사용자는 막히지 않는다, 보안 점검 2026-11-27 #4).
//! 여러 사용자가 한 IP 로 오는 범위(shared_ranges.txt, Claude 커넥터)는 IP 칸·전체 상한 대신 제 몫 분당 200 만 센다.
//! 그 몫은 한 묶음이고 하루 상한이 없다 (Claude 커넥터 사용자 모두가 나눠 쓴다) — 지금 쓰는 양에는 충분하다고 보고 둔다.
//! IP 는 메모리의 칸 열쇠로만 쓰고 어디에도 남기지 않는다. 다시 띄우면 센 수는 사라진다.

use std::collections::VecDeque;
use std::net::IpAddr;
use std::time::Instant;

use ipnet::IpNet;

use crate::assets::Limits;
use crate::capped::Capped;

/// 오래된 칸 치우기는 많아야 이 초에 한 번 (파이썬 PURGE_SECONDS, 보안 점검 2026-11-27 N1)
const PURGE_SECONDS: f64 = 10.0;
/// 칸 표(IP·망)마다 칸 수 상한 (파이썬 MAX_ENTRIES). 칸은 허락한 요청에만 만들고 (N1), 그래도 넘으면 가장 먼저 넣은 칸부터 뺀다 (#7)
pub const MAX_ENTRIES: usize = 100_000;
const DAY: f64 = 86400.0;

/// 하루 칸: (시작, 수) — 첫 부름부터 24시간을 센다 (때 목록 대신 두 값, #7)
type DayWindow = (f64, usize);

pub struct Limiter {
    started: Instant,
    minute: Capped<VecDeque<f64>>,
    day: Capped<DayWindow>,
    group_minute: Capped<VecDeque<f64>>,
    group_day: Capped<DayWindow>,
    total: VecDeque<f64>,
    shared: VecDeque<f64>,
    last_purge: Option<f64>,
}

impl Default for Limiter {
    fn default() -> Self {
        Self::new()
    }
}

/// 분 칸(없으면 빈 칸)의 수가 상한에 닿았으면 기다릴 초
fn minute_wait(window: Option<&VecDeque<f64>>, limit: usize, now: f64) -> Option<u64> {
    let window = window?;
    // 파이썬 max(1, int(window[0] + span - now) + 1): int() 는 0 쪽으로 자른다
    (limit > 0 && window.len() >= limit).then(|| 1.max((window[0] + 60.0 - now) as i64 + 1) as u64)
}

fn day_wait(window: Option<&DayWindow>, limit: usize, now: f64) -> Option<u64> {
    let window = window?;
    (limit > 0 && window.1 >= limit).then(|| 1.max((window.0 + DAY - now) as i64 + 1) as u64)
}

impl Limiter {
    pub fn new() -> Self {
        Self::with_cap(MAX_ENTRIES)
    }

    pub fn with_cap(cap: usize) -> Self {
        Self {
            started: Instant::now(),
            minute: Capped::new(cap),
            day: Capped::new(cap),
            group_minute: Capped::new(cap),
            group_day: Capped::new(cap),
            total: VecDeque::new(),
            shared: VecDeque::new(),
            last_purge: None,
        }
    }

    /// 허락하면 None (그리고 한 번 센다), 막으면 기다릴 초. 막힌 요청은 세지 않고 칸도 만들지 않는다.
    pub fn check(&mut self, ip: &str, limits: &Limits, shared_ranges: &[IpNet]) -> Option<u64> {
        let now = self.started.elapsed().as_secs_f64();
        if self.last_purge.is_none_or(|last| now - last >= PURGE_SECONDS) {
            self.purge(now);
            self.last_purge = Some(now);
        }
        if in_ranges(ip, shared_ranges) {
            // 공유 몫만 (전체 상한에 넣지 않는다)
            trim(&mut self.shared, now - 60.0);
            if let Some(wait) = minute_wait(Some(&self.shared), limits.shared_per_minute, now) {
                return Some(wait);
            }
            self.shared.push_back(now);
            return None;
        }
        let (host, net, factor) = (host_key(ip), network_key(ip), limits.group_factor);
        // 있는 칸은 그대로 손보고, 없는 칸은 빈 값으로 본다 — 표에는 허락한 뒤에만 넣는다 (파이썬과 같다)
        for window in [self.minute.map.get_mut(&host), self.group_minute.map.get_mut(&net)]
            .into_iter()
            .flatten()
        {
            trim(window, now - 60.0);
        }
        for window in [self.day.map.get_mut(&host), self.group_day.map.get_mut(&net)]
            .into_iter()
            .flatten()
        {
            if now - window.0 >= DAY {
                *window = (now, 0);
            }
        }
        trim(&mut self.total, now - 60.0);
        let group_minute = self.group_minute.map.get(&net);
        // 전체 상한은 이미 IP 한도만큼 쓴 망만 막는다 (파이썬 limits.py, 보안 점검 2026-11-27 #4): 그 아래 망은 상한 칸을 0 으로 보아 지나간다
        let total_limit = if group_minute.map_or(0, VecDeque::len) >= limits.per_minute {
            limits.total_per_minute
        } else {
            0
        };
        // 막는 차례: IP 분·IP 하루·망 분·망 하루·전체 (기다릴 초는 처음 막은 칸의 것)
        let wait = minute_wait(self.minute.map.get(&host), limits.per_minute, now)
            .or_else(|| day_wait(self.day.map.get(&host), limits.per_day, now))
            .or_else(|| minute_wait(group_minute, limits.per_minute * factor, now))
            .or_else(|| day_wait(self.group_day.map.get(&net), limits.per_day * factor, now))
            .or_else(|| minute_wait(Some(&self.total), total_limit, now));
        if wait.is_some() {
            return wait;
        }
        insert_capped(&mut self.minute, &host, VecDeque::new());
        insert_capped(&mut self.day, &host, (now, 0));
        insert_capped(&mut self.group_minute, &net, VecDeque::new());
        insert_capped(&mut self.group_day, &net, (now, 0));
        self.minute.map.get_mut(&host).expect("방금 넣은 칸").push_back(now);
        self.day.map.get_mut(&host).expect("방금 넣은 칸").1 += 1;
        self.group_minute.map.get_mut(&net).expect("방금 넣은 칸").push_back(now);
        self.group_day.map.get_mut(&net).expect("방금 넣은 칸").1 += 1;
        self.total.push_back(now);
        None
    }

    fn purge(&mut self, now: f64) {
        for book in [&mut self.minute, &mut self.group_minute] {
            book.retain(|hits| hits.back().is_some_and(|last| *last >= now - 60.0));
        }
        for book in [&mut self.day, &mut self.group_day] {
            book.retain(|(start, _)| now - *start < DAY);
        }
    }

    /// 시험용: 칸 표마다 칸 수 (IP 분·IP 하루·망 분·망 하루)
    pub fn sizes(&self) -> [usize; 4] {
        [self.minute.len(), self.day.len(), self.group_minute.len(), self.group_day.len()]
    }
}

fn insert_capped<V>(book: &mut Capped<V>, name: &str, value: V) {
    if book.map.contains_key(name) {
        return;
    }
    while book.len() >= book.cap {
        book.pop_oldest();
    }
    book.put(name.to_string(), value);
}

fn trim(hits: &mut VecDeque<f64>, before: f64) {
    while hits.front().is_some_and(|first| *first < before) {
        hits.pop_front();
    }
}

pub(crate) fn in_ranges(ip: &str, ranges: &[IpNet]) -> bool {
    // 파이썬 `address in network`: 판(v4·v6)이 다르면 들지 않는다 (::ffff: 를 풀지 않는다)
    ip.parse::<IpAddr>()
        .is_ok_and(|address| ranges.iter().any(|range| range.contains(&address)))
}

/// 한 기계의 칸: IPv4 그대로, IPv6 /64, ::ffff: 는 IPv4. 주소가 아니면('direct') 그대로.
pub fn host_key(ip: &str) -> String {
    prefix(ip, 32, 64)
}

/// 한 망의 칸: IPv4 /24, IPv6 /48.
pub fn network_key(ip: &str) -> String {
    prefix(ip, 24, 48)
}

fn prefix(ip: &str, v4: u8, v6: u8) -> String {
    let Ok(address) = ip.parse::<IpAddr>() else { return ip.to_string() };
    let address = match address {
        IpAddr::V6(v6addr) => v6addr.to_ipv4_mapped().map_or(address, IpAddr::V4),
        v4addr => v4addr,
    };
    let bits = if address.is_ipv4() { v4 } else { v6 };
    IpNet::new(address, bits).expect("접두 길이").trunc().to_string()
}

/// 역방향 프록시가 넣은 X-Forwarded-For 의 첫 값 (파이썬 client_ip). 없으면 None.
pub fn client_ip(forwarded: Option<&str>) -> Option<String> {
    let first = forwarded?.split(',').next()?.trim_matches(crate::text::is_py_space);
    (!first.is_empty()).then(|| first.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> Limits {
        Limits {
            per_minute: 30,
            per_day: 300,
            total_per_minute: 300,
            shared_per_minute: 200,
            group_factor: 3,
        }
    }

    #[test]
    fn thirty_first_call_in_a_minute_is_refused() {
        let mut limiter = Limiter::new();
        for _ in 0..30 {
            assert_eq!(limiter.check("198.18.1.1", &limits(), &[]), None);
        }
        assert!(
            limiter
                .check("198.18.1.1", &limits(), &[])
                .is_some_and(|wait| (1..=61).contains(&wait))
        );
        assert_eq!(limiter.check("198.18.2.1", &limits(), &[]), None);
    }

    #[test]
    fn same_network_shares_three_times_the_limit() {
        let mut limiter = Limiter::new();
        for n in 0..90 {
            assert_eq!(limiter.check(&format!("198.18.1.{}", n % 4 + 1), &limits(), &[]), None, "{n}");
        }
        assert!(limiter.check("198.18.1.9", &limits(), &[]).is_some());
    }

    #[test]
    fn keys_match_python() {
        assert_eq!(host_key("::ffff:1.2.3.4"), "1.2.3.4/32");
        assert_eq!(host_key("2001:db8:1:2:3::9"), "2001:db8:1:2::/64");
        assert_eq!(network_key("1.2.3.4"), "1.2.3.0/24");
        assert_eq!(host_key("direct"), "direct");
    }

    #[test]
    fn shared_range_counts_only_the_shared_share() {
        // 파이썬 test_claude_connector_ranges_skip_the_per_ip_limit_and_have_their_own_share
        let ranges: Vec<IpNet> = ["160.79.104.0/21", "153.61.208.0/20"].iter().map(|r| r.parse().unwrap()).collect();
        let limits = Limits {
            per_minute: 2,
            per_day: 300,
            total_per_minute: 3,
            shared_per_minute: 4,
            group_factor: 3,
        };
        let mut limiter = Limiter::new();
        assert!((0..4).all(|_| limiter.check("160.79.105.7", &limits, &ranges).is_none()));
        assert!(limiter.check("153.61.210.1", &limits, &ranges).is_some());
        assert!(limiter.check("203.0.113.5", &limits, &ranges).is_none() && limiter.check("198.51.100.6", &limits, &ranges).is_none());
    }

    #[test]
    fn heavy_networks_filling_the_total_cannot_lock_out_light_users_or_claude() {
        // 파이썬 test_a_few_heavy_networks_filling_the_total_cannot_lock_out_light_users_or_claude
        let ranges: Vec<IpNet> = ["160.79.104.0/21"].iter().map(|r| r.parse().unwrap()).collect();
        let limits = limits();
        let mut limiter = Limiter::new();
        for net in 0..10 {
            assert!((0..30).all(|_| limiter.check(&format!("198.18.{net}.1"), &limits, &ranges).is_none()));
        }
        assert!(limiter.check("198.18.0.2", &limits, &ranges).is_some());
        assert!(limiter.check("203.0.113.9", &limits, &ranges).is_none());
        assert!(limiter.check("160.79.105.7", &limits, &ranges).is_none());
    }

    #[test]
    fn day_windows_count_and_tables_are_capped() {
        // 파이썬 test_day_windows_are_two_numbers_and_tables_have_a_hard_cap (시계를 못 돌려 상한·수만 본다)
        let day_only = Limits {
            per_minute: 0,
            per_day: 3,
            total_per_minute: 0,
            shared_per_minute: 0,
            group_factor: 3,
        };
        let mut limiter = Limiter::new();
        assert!((0..3).all(|_| limiter.check("a", &day_only, &[]).is_none()));
        assert!(limiter.check("a", &day_only, &[]).is_some_and(|wait| wait > 86000));
        let mut capped = Limiter::with_cap(5);
        for n in 0..20 {
            assert!(capped.check(&format!("10.{n}.0.1"), &limits(), &[]).is_none());
        }
        assert!(capped.sizes().iter().all(|size| *size <= 5), "{:?}", capped.sizes());
    }

    #[test]
    fn refused_requests_and_rotation_do_not_grow_the_tables() {
        // 파이썬 test_refused_requests_and_address_rotation_do_not_grow_the_tables (N1)
        let rotate = Limits {
            per_minute: 2,
            per_day: 0,
            total_per_minute: 0,
            shared_per_minute: 0,
            group_factor: 3,
        };
        let mut limiter = Limiter::new();
        let allowed = (0..50)
            .filter(|n| limiter.check(&format!("2001:db8:7:{n:x}::1"), &rotate, &[]).is_none())
            .count();
        assert_eq!(allowed, 6);
        assert_eq!(limiter.sizes(), [6, 6, 1, 1]);
    }
}
