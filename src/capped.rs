//! 넣은 차례를 기억하는 표 (파이썬 dict 처럼): 칸 수 상한을 넘으면 가장 먼저 넣은 칸부터 뺀다. 키 확인 담기와 IP 한도 칸이 같이 쓴다.

use std::collections::{HashMap, VecDeque};

pub struct Capped<V> {
    pub map: HashMap<String, V>,
    order: VecDeque<String>,
    pub cap: usize,
}

impl<V> Capped<V> {
    pub fn new(cap: usize) -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            cap,
        }
    }

    /// 넣고, cap 을 넘으면 가장 먼저 넣은 것 하나를 뺀다 (이미 있는 열쇠는 자리를 그대로 둔다)
    pub fn put(&mut self, name: String, value: V) {
        if self.map.insert(name.clone(), value).is_none() {
            self.order.push_back(name);
        }
        if self.map.len() > self.cap {
            self.pop_oldest();
        }
    }

    pub fn pop_oldest(&mut self) {
        if let Some(oldest) = self.order.pop_front() {
            self.map.remove(&oldest);
        }
    }

    pub fn remove(&mut self, name: &str) {
        if self.map.remove(name).is_some() {
            self.order.retain(|n| n != name);
        }
    }

    /// keep 이 false 인 칸을 뺀다 (차례도 함께)
    pub fn retain(&mut self, keep: impl Fn(&V) -> bool) {
        self.map.retain(|_, v| keep(v));
        let map = &self.map;
        self.order.retain(|n| map.contains_key(n));
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}
