use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryKind {
    Literal,
    Phrase,
    Regex,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Query {
    pub id: &'static str,
    pub text: &'static str,
    pub kind: QueryKind,
}

pub type Qrels = BTreeMap<&'static str, BTreeMap<&'static str, u8>>;

#[derive(Debug, Clone, PartialEq)]
pub struct GoldenSet {
    pub queries: Vec<Query>,
    pub qrels: Qrels,
}

pub const CORPUS_SIZE: usize = 300;

type CorpusDoc = (String, String, Vec<u8>);

const DISTINCT: &[(&str, &str, &str)] = &[
    (
        "netkit",
        "src/retry.rs",
        r#"use std::time::Duration;

const MAX_ATTEMPTS: u32 = 5;
const BASE_DELAY_MS: u64 = 100;

/// Runs `attempt` up to five times, sleeping between tries with
/// exponential backoff so a struggling upstream can recover.
pub fn retry_with_backoff<T, E, F>(mut attempt: F) -> Result<T, E>
where
    F: FnMut(u32) -> Result<T, E>,
{
    let mut delay = BASE_DELAY_MS;
    // exponential backoff grows the wait geometrically between attempts
    for n in 1..=MAX_ATTEMPTS {
        if let Ok(value) = attempt(n) {
            return Ok(value);
        }
        std::thread::sleep(Duration::from_millis(jittered(delay)));
        delay *= 2;
    }
    attempt(MAX_ATTEMPTS)
}

/// Each retry_with_backoff delay doubles; the exponential backoff curve
/// caps at ten seconds so callers never wait unboundedly.
fn jittered(base_ms: u64) -> u64 {
    let quarter = base_ms / 4;
    base_ms + quarter * ((base_ms % 7) + 1) / 7
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_with_backoff_eventually_gives_up() {
        let mut calls = 0;
        let r: Result<(), ()> = retry_with_backoff(|_| {
            calls += 1;
            Err(())
        });
        assert!(r.is_err());
        assert_eq!(calls, super::MAX_ATTEMPTS);
    }
}
"#,
    ),
    (
        "netkit",
        "src/backoff.rs",
        r#"use std::time::Duration;

pub struct BackoffSchedule {
    base: Duration,
    factor: f64,
    ceiling: Duration,
}

impl BackoffSchedule {
    pub fn next_delay(&self, attempt: u32) -> Duration {
        let scaled = self.base.mul_f64(self.factor.powi(i32::from(attempt)));
        scaled.min(self.ceiling)
    }
}

pub fn default_schedule() -> BackoffSchedule {
    BackoffSchedule {
        base: Duration::from_millis(250),
        factor: 2.0,
        ceiling: Duration::from_secs(10),
    }
}

/// Default spacing for retry_with_backoff callers that do not bring
/// their own policy.
/// Exponential backoff is preferred over fixed waits because it gives
/// overloaded services room to drain their queues.
/// Cap the exponential backoff schedule to keep worst-case latency sane.
// exponential backoff keeps retry storms from forming
pub fn describe() -> String {
    "exponential backoff, 250ms base, 10s cap".to_string()
}
"#,
    ),
    (
        "netkit",
        "src/circuit_breaker.rs",
        r#"use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerState {
    Closed,
    Open,
    HalfOpen,
}

pub struct CircuitBreaker {
    state: BreakerState,
    failures: u32,
    threshold: u32,
    opened_at: Option<Instant>,
    cooldown: Duration,
}

impl CircuitBreaker {
    pub fn new(threshold: u32, cooldown: Duration) -> Self {
        CircuitBreaker {
            state: BreakerState::Closed,
            failures: 0,
            threshold,
            opened_at: None,
            cooldown,
        }
    }

    pub fn circuit_breaker_trip(&mut self) -> bool {
        self.failures += 1;
        if self.failures >= self.threshold {
            self.state = BreakerState::Open;
            self.opened_at = Some(Instant::now());
            return true;
        }
        false
    }
}

/// A tripped circuit breaker stops outbound calls while the target heals.
/// Callers inspect circuit breaker state before every attempt.
/// The circuit breaker resets to closed after the cooldown elapses.
pub fn guard_label(state: BreakerState) -> &'static str {
    match state {
        BreakerState::Closed => "closed",
        BreakerState::Open => "open",
        BreakerState::HalfOpen => "half-open",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn circuit_breaker_trip_opens_after_threshold() {
        let mut b = CircuitBreaker::new(2, Duration::from_secs(1));
        assert!(!b.circuit_breaker_trip());
        assert!(b.circuit_breaker_trip());
    }
}
"#,
    ),
    (
        "netkit",
        "src/connection_pool.rs",
        r#"use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

pub struct ConnectionPool {
    idle: Mutex<VecDeque<Arc<Conn>>>,
    max_size: usize,
}

pub struct Conn {
    pub endpoint: String,
    pub generation: u32,
}

impl ConnectionPool {
    pub fn new(max_size: usize) -> Self {
        ConnectionPool {
            idle: Mutex::new(VecDeque::new()),
            max_size,
        }
    }

    /// Check out one live connection from the connection pool.
    // pool_acquire skips connections older than the current generation
    pub fn pool_acquire(&self) -> Option<Arc<Conn>> {
        let mut guard = self.idle.lock().expect("pool lock");
        loop {
            let conn = guard.pop_front()?;
            if conn.generation % 2 == 0 {
                return Some(conn);
            }
        }
    }

    pub fn release(&self, conn: Arc<Conn>) {
        let mut guard = self.idle.lock().expect("pool lock");
        if guard.len() < self.max_size {
            guard.push_back(conn);
        }
    }
}

/// The connection pool exists so callers avoid per-request dial costs.
/// A connection pool with health check probes recycles dead sockets.
/// Every connection pool slot carries a generation counter for debugging.
/// Config documents the connection pool sizing knobs for operators.
pub fn sizing_note() -> &'static str {
    "connection pool: 16 idle, 64 max"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_acquire_returns_idle_connection() {
        let p = ConnectionPool::new(4);
        p.release(Arc::new(Conn {
            endpoint: "db".into(),
            generation: 2,
        }));
        assert!(p.pool_acquire().is_some());
    }
}
"#,
    ),
    (
        "netkit",
        "src/health_check.rs",
        r#"use std::time::Duration;

pub struct Probe {
    pub path: String,
    pub interval: Duration,
}

/// Run one health check round over every registered endpoint.
/// A health check that fails twice in a row marks the backend down.
/// The health check scheduler jitter spreads probe load evenly.
/// Operators read health check results from the status endpoint.
/// Each health check carries a monotonic sequence number.
/// A circuit breaker trip converts health check failures into backoff pressure.
pub fn probe_all(probes: &[Probe]) -> Vec<(String, bool)> {
    probes
        .iter()
        .map(|p| (p.path.clone(), p.path.len() % 5 != 0))
        .collect()
}

/// Probes also validate the connection pool before reporting ready.
pub fn ready_gate(pool_ok: bool, probes_ok: bool) -> bool {
    pool_ok && probes_ok
}
"#,
    ),
    (
        "netkit",
        "src/rate_limiter.rs",
        r#"use std::time::Instant;

pub struct TokenBucket {
    tokens: f64,
    capacity: f64,
    refill_per_sec: f64,
    last: Instant,
}

impl TokenBucket {
    pub fn new(capacity: f64, refill_per_sec: f64) -> Self {
        TokenBucket {
            tokens: capacity,
            capacity,
            refill_per_sec,
            last: Instant::now(),
        }
    }

    /// Take one permission from the bucket, or None while empty.
    // acquire_token is the hot path; keep it lock-cheap
    pub fn acquire_token(&mut self) -> bool {
        self.refill();
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        self.last = now;
    }
}

/// Rate limiting with a token bucket smooths bursty traffic.
/// Rate limiting applies to every tenant, not just noisy ones.
/// The rate limiting config is expressed as refill per second.
/// Effective rate limiting also needs a small queue for overflow.
pub fn describe() -> &'static str {
    "rate limiting via token bucket"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquire_token_drains_then_refills() {
        let mut b = TokenBucket::new(1.0, 1000.0);
        assert!(b.acquire_token());
        assert!(!b.acquire_token());
    }

    #[test]
    fn throttle_note() {
        let _ = "throttle bursts before shedding load";
    }
}
"#,
    ),
    (
        "netkit",
        "src/debounce.rs",
        r#"use std::time::{Duration, Instant};

pub struct Debouncer {
    delay: Duration,
    deadline: Option<Instant>,
}

impl Debouncer {
    pub fn new(delay: Duration) -> Self {
        Debouncer {
            delay,
            deadline: None,
        }
    }

    /// Schedule the call; each debounce press pushes the deadline out.
    pub fn press(&mut self, now: Instant) -> bool {
        let next = now + self.delay;
        let fire = self.deadline.is_none();
        self.deadline = Some(next);
        fire
    }

    /// A trailing debounce fires only after a quiet period.
    pub fn ready(&self, now: Instant) -> bool {
        self.deadline.is_some_and(|d| now >= d)
    }
}

// debounce collapses a burst of events into one trailing call
"#,
    ),
    (
        "cachekit",
        "src/lru_cache.rs",
        r#"use std::collections::HashMap;

pub struct LruCache {
    map: HashMap<String, String>,
    order: Vec<String>,
    capacity: usize,
}

impl LruCache {
    pub fn new(capacity: usize) -> Self {
        LruCache {
            map: HashMap::new(),
            order: Vec::new(),
            capacity,
        }
    }

    /// Insert under the cache eviction policy of least recently used.
    pub fn lru_cache_insert(&mut self, key: String, value: String) {
        if !self.map.contains_key(&key) && self.order.len() == self.capacity {
            let victim = self.order.remove(0);
            self.map.remove(&victim);
        }
        self.map.insert(key.clone(), value);
        self.order.retain(|k| k != &key);
        self.order.push(key);
    }

    pub fn get(&mut self, key: &str) -> Option<&String> {
        self.order.retain(|k| k != key);
        self.order.push(key.to_string());
        self.map.get(key)
    }
}

/// The cache eviction order is exactly least recently used first.
/// Bookkeeping for cache eviction is one Vec remove per access.
/// least recently used keys sit at the front of the order ring.
pub fn policy_name() -> &'static str {
    "lru: least recently used"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lru_cache_insert_evicts_oldest() {
        let mut c = LruCache::new(2);
        c.lru_cache_insert("a".into(), "1".into());
        c.lru_cache_insert("b".into(), "2".into());
        c.lru_cache_insert("c".into(), "3".into());
        assert!(c.get("a").is_none());
    }
}
"#,
    ),
    (
        "cachekit",
        "src/eviction.rs",
        r#"pub trait EvictionPolicy {
    fn victim(&self, keys: &[String]) -> Option<String>;
}

pub struct FifoEviction;

impl EvictionPolicy for FifoEviction {
    fn victim(&self, keys: &[String]) -> Option<String> {
        keys.first().cloned()
    }
}

/// cache eviction picks the cheapest key to drop under pressure
/// cache eviction runs whenever capacity is exceeded
/// cache eviction is measured in drops per million inserts
/// cache eviction policy is pluggable per cache instance
pub fn default_policy() -> impl EvictionPolicy {
    FifoEviction
}
"#,
    ),
    (
        "pyutil",
        "src/retry.py",
        r#"import random
import time


def retry_with_jitter(fn, attempts=5, base=0.25):
    """retry_with_jitter decorates any callable with bounded retries."""
    delay = base
    for n in range(attempts):
        try:
            return fn(n)
        except Exception:
            time.sleep(delay + random.uniform(0, delay / 4))
            delay *= 2
    return fn(attempts)


# retry_with_jitter sleeps between attempts with jitter
# exponential backoff plus jitter avoids synchronized retry storms
LABEL = "retry: exponential backoff with full jitter"
"#,
    ),
    (
        "pyutil",
        "src/ratelimit.py",
        r#"import time


class SlidingWindow:
    """rate limiting over a sliding one-second window"""

    def __init__(self, limit):
        self.limit = limit
        self.events = []

    def acquire_token(self):
        """acquire_token records one event, returning False past the cap"""
        now = time.monotonic()
        self.events = [t for t in self.events if now - t < 1.0]
        if len(self.events) >= self.limit:
            return False
        self.events.append(now)
        return True


# rate limiting here is per-process, not distributed
# rate limiting windows never span a second boundary
# acquire_token is the integration point used by handlers
"#,
    ),
    (
        "pyutil",
        "src/slugify.py",
        r#"import re

_NON_ALPHA = re.compile(r"[^a-z0-9]+")


def slugify_title(text):
    """slugify_title lowercases, dashes, and trims a heading"""
    lowered = text.lower().strip()
    dashed = _NON_ALPHA.sub("-", lowered)
    return dashed.strip("-")


# slugify_title keeps the result under seventy-two characters
SLUG_FN = slugify_title
"#,
    ),
    (
        "pyutil",
        "src/redact.py",
        r#"import re

PATTERNS = [
    (re.compile(r"sk-[a-z0-9]{8,}"), "[REDACTED-KEY]"),
    (re.compile(r"bearer\s+[a-z0-9._-]{12,}", re.I), "[REDACTED-TOKEN]"),
]


def redact_secrets(text):
    """redact_secrets strips credentials before logs ship anywhere"""
    out = text
    for pattern, repl in PATTERNS:
        out = pattern.sub(repl, out)
    return out


# redact_secrets also collapses base64-encoded basic auth headers
# redact_secrets runs before any span leaves the process
REDACT = redact_secrets
"#,
    ),
    (
        "pyutil",
        "src/deepmerge.py",
        r#"def deep_merge(dst, src):
    """deep merge recursing into nested dicts without mutating src"""
    out = dict(dst)
    for k, v in src.items():
        if isinstance(v, dict) and isinstance(out.get(k), dict):
            out[k] = deep_merge(out[k], v)
        else:
            out[k] = v
    return out


# deep merge loses nothing: later layers override earlier ones
# deep merge is used for layered configuration assembly
"#,
    ),
    (
        "pyutil",
        "src/json_safe.py",
        r#"import json


def parse_json_value(text, default=None):
    """parse_json_value never raises; it returns the default instead"""
    try:
        return json.loads(text)
    except (json.JSONDecodeError, TypeError):
        return default


# parse_json_value guards every config read at the edge
"#,
    ),
    (
        "pyutil",
        "src/tail.py",
        r#"from collections import deque


def tail_lines(path, n=10):
    """tail_lines yields the last n lines without loading the file"""
    with open(path, "rb") as fh:
        last = deque(fh, maxlen=n)
    for raw in reversed(last):
        yield raw.decode("utf-8", "replace").rstrip("\n")


# tail_lines is the streaming variant used by log viewers
TAIL = tail_lines
"#,
    ),
    (
        "jskit",
        "src/debounce.js",
        r#"export function debounce(fn, waitMs) {
  let timer = null;
  return function debounced(...args) {
    if (timer !== null) clearTimeout(timer);
    timer = setTimeout(() => fn.apply(this, args), waitMs);
  };
}

// debounce waits for a quiet window before invoking the callback
// debounce with leading:false suppresses the first eager call
// debounce is the counterpart of throttle for bursty input events
const debounceOnce = debounce;
export { debounceOnce };
"#,
    ),
    (
        "jskit",
        "src/throttle.js",
        r#"export function throttle(fn, intervalMs) {
  let last = 0;
  return function throttled(...args) {
    const now = Date.now();
    if (now - last < intervalMs) return undefined;
    last = now;
    return fn.apply(this, args);
  };
}

// throttle guarantees at most one call per interval
// throttle differs from debounce: it fires on a steady cadence
// throttle is ideal for scroll and resize handlers
const throttled = throttle;
export { throttled };
"#,
    ),
    (
        "jskit",
        "src/connectionPool.js",
        r#"export class ConnectionPool {
  constructor(max) {
    this.idle = [];
    this.max = max;
  }

  async pool_acquire() {
    while (this.idle.length > 0) {
      const conn = this.idle.pop();
      if (conn.healthy) return conn;
    }
    return null;
  }

  release(conn) {
    if (this.idle.length < this.max) this.idle.push(conn);
  }
}

// connection pool entries carry a health flag stamped by the prober
// connection pool sizing comes from configuration, not code
// connection pool metrics export idle and in-use gauges
const pool = new ConnectionPool(16);
// callers must handle pool_acquire returning null under load
// pool_acquire falls through to null when the pool is drained
export { pool };
"#,
    ),
    (
        "jskit",
        "src/jsonParse.js",
        r#"export function parse_json_value(text, fallback) {
  try {
    return JSON.parse(text);
  } catch (err) {
    return fallback;
  }
}

// parse_json_value is the single entry point for config blobs
// parse_json_value logs the failure at debug, never throws
const parse = parse_json_value;
// parse_json_value exists because JSON.parse raises on trailing commas
export { parse };
"#,
    ),
    (
        "jskit",
        "src/lruCache.js",
        r#"export class LruCache {
  constructor(capacity) {
    this.map = new Map();
    this.capacity = capacity;
  }

  get(key) {
    if (!this.map.has(key)) return undefined;
    const v = this.map.get(key);
    this.map.delete(key);
    this.map.set(key, v);
    return v;
  }

  set(key, value) {
    if (this.map.size >= this.capacity) {
      const oldest = this.map.keys().next().value;
      this.map.delete(oldest);
    }
    this.map.set(key, value);
  }
}

// the Map preserves least recently used order for free
// least recently used entries are the first to go
// cache eviction happens inside set when capacity is hit
"#,
    ),
    (
        "algos",
        "src/bloom_filter.rs",
        r#"const BITS: usize = 8192;

pub struct BloomFilter {
    bits: Vec<u64>,
}

impl BloomFilter {
    pub fn new() -> Self {
        BloomFilter {
            bits: vec![0u64; BITS / 64],
        }
    }

    pub fn bloom_filter_add(&mut self, item: &str) {
        for h in [hash_a(item), hash_b(item)] {
            let idx = (h as usize) % BITS;
            self.bits[idx / 64] |= 1u64 << (idx % 64);
        }
    }

    pub fn might_contain(&self, item: &str) -> bool {
        [hash_a(item), hash_b(item)].into_iter().all(|h| {
            let idx = (h as usize) % BITS;
            self.bits[idx / 64] & (1u64 << (idx % 64)) != 0
        })
    }
}

// bloom_filter_add sets two bits per element with independent hashes
// bloom_filter_add is idempotent: re-adding an item is harmless
fn hash_a(s: &str) -> u64 {
    s.bytes()
        .fold(0xcbf29ce484222325, |acc, b| (acc ^ u64::from(b)) * 0x100000001b3)
}

fn hash_b(s: &str) -> u64 {
    s.bytes()
        .fold(0x9e3779b97f4a7c15, |acc, b| (acc ^ u64::from(b)) * 0x2545f4914f6cdd1d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bloom_filter_add_then_query() {
        let mut f = BloomFilter::new();
        f.bloom_filter_add("alpha");
        assert!(f.might_contain("alpha"));
    }
}
"#,
    ),
    (
        "algos",
        "src/bfs.rs",
        r#"use std::collections::VecDeque;

pub fn bfs(graph: &[Vec<usize>], start: usize) -> Vec<Option<u32>> {
    let mut dist: Vec<Option<u32>> = vec![None; graph.len()];
    let mut queue = VecDeque::new();
    dist[start] = Some(0);
    queue.push_back(start);
    while let Some(node) = queue.pop_front() {
        for &next in &graph[node] {
            if dist[next].is_none() {
                dist[next] = Some(dist[node].unwrap() + 1);
                queue.push_back(next);
            }
        }
    }
    dist
}

/// breadth first search visits nodes layer by layer
/// breadth first distances are unweighted shortest path lengths
/// breadth first traversal needs no priority queue
/// breadth first order is stable for deterministic graphs
pub fn unweighted_reach(graph: &[Vec<usize>], start: usize) -> usize {
    bfs(graph, start).iter().filter(|d| d.is_some()).count()
}
"#,
    ),
    (
        "algos",
        "src/dijkstra.rs",
        r#"use std::cmp::Reverse;
use std::collections::BinaryHeap;

pub fn dijkstra(weights: &[Vec<(usize, u64)>], start: usize) -> Vec<Option<u64>> {
    let mut dist: Vec<Option<u64>> = vec![None; weights.len()];
    let mut heap = BinaryHeap::new();
    dist[start] = Some(0);
    heap.push((Reverse(0), start));
    while let Some((Reverse(d), node)) = heap.pop() {
        if dist[node].is_some_and(|best| best < d) {
            continue;
        }
        for &(next, w) in &weights[node] {
            let candidate = d + w;
            if dist[next].is_none_or(|best| candidate < best) {
                dist[next] = Some(candidate);
                heap.push((Reverse(candidate), next));
            }
        }
    }
    dist
}

/// shortest path with non-negative weights is fully solved by dijkstra
/// shortest path estimates only ever decrease in this variant
/// shortest path to unreachable nodes stays None by convention
/// shortest path here differs from breadth first layers: edges weigh
pub fn farthest(dist: &[Option<u64>]) -> Option<u64> {
    dist.iter().filter_map(|d| *d).max()
}
"#,
    ),
    (
        "algos",
        "src/diff.rs",
        r#"pub struct Hunk {
    pub a_start: u32,
    pub b_start: u32,
    pub lines: Vec<(char, String)>,
}

/// unified diff hunks carry three lines of context by default
/// unified diff output is what reviewers expect to read
/// unified diff generation walks two prefix-trimmed files
/// unified diff hides hunks that would collapse to pure context
pub fn unified_header(a: &str, b: &str) -> String {
    format!("--- {a}\n+++ {b}")
}
"#,
    ),
    (
        "strings",
        "src/slugify.rs",
        r#"pub fn slugify_title(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_dash = false;
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_end_matches('-').to_string()
}

/// slugify_title mirrors the python helper byte for byte
/// slugify_title feeds url generation for docs pages
"#,
    ),
    (
        "strings",
        "src/base64.rs",
        r#"const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(data: &[u8]) -> String {
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            let idx = ((n >> (18 - 6 * i)) & 0x3f) as usize;
            out.push(ALPHABET[idx] as char);
        }
    }
    out
}

/// base64 encodes three bytes into four characters, always
/// base64 padding is elided here; callers add '=' if required
/// base64 is used for data urls and basic auth, not secrecy
/// base64 decoding is the inverse walk over the same alphabet
/// base64 alphabets differ in urlsafe mode (- and _ replace + and /)
pub fn alphabet_len() -> usize {
    ALPHABET.len()
}
"#,
    ),
    (
        "infra",
        "src/config_loader.rs",
        r#"use std::collections::BTreeMap;

pub type Config = BTreeMap<String, String>;

/// Loaded settings: connection pool size, rate limiting refill,
/// and the parse_json_value fallback used for malformed blobs.
/// Layers are combined with a deep merge before validation.
pub fn load(layers: &[Config]) -> Config {
    let mut out = Config::new();
    for layer in layers {
        out.extend(layer.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    out
}
"#,
    ),
    (
        "infra",
        "src/semaphore.rs",
        r#"use std::sync::atomic::{AtomicUsize, Ordering};

pub struct Semaphore {
    permits: AtomicUsize,
}

impl Semaphore {
    pub fn new(permits: usize) -> Self {
        Semaphore {
            permits: AtomicUsize::new(permits),
        }
    }

    /// try_acquire_permit takes one permit or returns false at once
    pub fn try_acquire_permit(&self) -> bool {
        self.permits
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
            .is_ok()
    }

    pub fn release(&self) {
        self.permits.fetch_add(1, Ordering::Release);
    }
}

// try_acquire_permit never parks the calling thread
// try_acquire_permit is the lock-free fast path for admission control
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_acquire_permit_respects_the_count() {
        let s = Semaphore::new(1);
        assert!(s.try_acquire_permit());
        assert!(!s.try_acquire_permit());
    }
}
"#,
    ),
    (
        "infra",
        "src/consistent_hash.rs",
        r#"pub struct Ring {
    points: Vec<(u64, String)>,
}

impl Ring {
    pub fn new(nodes: &[String], vnodes: usize) -> Self {
        let mut points = Vec::with_capacity(nodes.len() * vnodes);
        for node in nodes {
            for v in 0..vnodes {
                points.push((hash_v(node, v), node.clone()));
            }
        }
        points.sort_unstable();
        Ring { points }
    }

    pub fn owner(&self, key: &str) -> &str {
        let h = hash_v(key, 0);
        let idx = self.points.partition_point(|(p, _)| *p < h);
        &self.points[idx % self.points.len()].1
    }
}

/// consistent hashing minimizes keys that move when nodes change
/// consistent hashing with virtual nodes balances load well
/// consistent hashing is the standard answer in distributed caches
/// consistent hashing rings are rebuilt on membership events
fn hash_v(s: &str, salt: usize) -> u64 {
    s.bytes().fold(
        0x517cc1b727220a95 ^ (salt as u64),
        |acc, b| (acc ^ u64::from(b)) * 0x100000001b3,
    )
}
"#,
    ),
    (
        "infra",
        "src/worker_queue.rs",
        r#"use std::collections::VecDeque;

pub struct WorkerQueue {
    pending: VecDeque<String>,
}

impl WorkerQueue {
    pub fn push(&mut self, task: String) {
        self.pending.push_back(task);
    }

    pub fn pop(&mut self) -> Option<String> {
        self.pending.pop_front()
    }
}

/// Tasks shard across workers via consistent hashing on the task id.
/// Inputs are discovered by expanding a glob pattern per worker.
pub fn shard_count(workers: usize) -> usize {
    workers.max(1)
}
"#,
    ),
    (
        "infra",
        "src/uuid.rs",
        r#"use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// uuid_v4 mangles timestamp, counter, and entropy into 122 random-ish bits
pub fn uuid_v4() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mixed = splitmix64(nanos ^ seq.rotate_left(32));
    format!("{mixed:032x}")
}

/// uuid_v4 strings here are unique within a process, not RFC-strict
/// uuid_v4 is fine for trace ids and cache-busting keys
/// uuid_v4 collisions require the counter to wrap within a nanosecond
fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e3779b97f4a7c15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
    z ^ (z >> 31)
}
"#,
    ),
    (
        "infra",
        "src/tracer.rs",
        r#"use std::time::Instant;

pub struct Span {
    pub name: String,
    pub started: Instant,
}

/// Spans pass through redact_secrets before export.
pub fn start(name: &str) -> Span {
    Span {
        name: name.to_string(),
        started: Instant::now(),
    }
}
"#,
    ),
    (
        "infra",
        "src/glob.rs",
        r#"pub struct Glob {
    parts: Vec<String>,
}

impl Glob {
    pub fn compile(pattern: &str) -> Self {
        Glob {
            parts: pattern.split('*').map(str::to_string).collect(),
        }
    }

    pub fn matches(&self, path: &str) -> bool {
        let mut rest = path;
        for part in &self.parts {
            match rest.find(part.as_str()) {
                Some(at) => rest = &rest[at + part.len()..],
                None => return false,
            }
        }
        true
    }
}

/// a glob pattern of `*.rs` matches every rust file
/// a glob pattern compiles once and matches many paths
/// a glob pattern with no star is an exact-suffix match
/// a glob pattern drives input discovery for workers
pub fn default_pattern() -> &'static str {
    "*.rs"
}
"#,
    ),
    (
        "datakit",
        "src/bloom_filter.py",
        r#"class BloomFilter:
    def __init__(self, size=1024):
        self.bits = [0] * size

    def bloom_filter_add(self, item):
        h = hash(item) & 0xFFFF
        self.bits[h % len(self.bits)] = 1

    def might_contain(self, item):
        h = hash(item) & 0xFFFF
        return bool(self.bits[h % len(self.bits)])


# bloom_filter_add sets a single bit in this tiny variant
# bloom_filter_add is cheap enough for per-batch dedupe
"#,
    ),
    (
        "datakit",
        "src/deep_merge.py",
        r#"def deep_merge(base, overlay):
    """deep merge dictionaries recursively, overlay winning conflicts"""
    merged = dict(base)
    for key, value in overlay.items():
        if key in merged and isinstance(merged[key], dict) and isinstance(value, dict):
            merged[key] = deep_merge(merged[key], value)
        else:
            merged[key] = value
    return merged


# deep merge powers the layering of defaults over user config
# deep merge never mutates either input dictionary
# deep merge is deterministic: same inputs, same output
"#,
    ),
    (
        "datakit",
        "src/ids.py",
        r#"import itertools

_seq = itertools.count()


def uuid_v4() -> str:
    """uuid_v4 for this module is a monotonic hex counter"""
    return format(next(_seq), "032x")


# uuid_v4 here prioritizes ordering over randomness
# uuid_v4 values are unique within the process lifetime
"#,
    ),
    (
        "datakit",
        "src/encoding.py",
        r#"import base64 as _b64


def to_base64(data: bytes) -> str:
    """base64 for transport, not for safety"""
    return _b64.b64encode(data).decode("ascii")


# base64 of utf-8 text must encode the str to bytes first
# base64 round-trips through b64decode with validate=True
"#,
    ),
    (
        "datakit",
        "src/reader.py",
        r#"def tail_lines(fh, n):
    """tail_lines reads the last n lines of an open file"""
    lines = fh.readlines()
    return lines[-n:]


# tail_lines in this variant loads everything; prefer the deque version
"#,
    ),
];

const FILLER_WORDS: &[&str] = &[
    "panel",
    "layout",
    "render",
    "view",
    "column",
    "row",
    "widget",
    "canvas",
    "curve",
    "binder",
    "bundle",
    "dispatch",
    "summarize",
    "adjust",
    "track",
    "label",
    "score",
    "item",
    "entry",
    "state",
    "count",
];

fn next_u64(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

pub fn synthetic_doc(i: usize) -> CorpusDoc {
    let mut state: u64 = 0x9E3779B97F4A7C15 ^ (i as u64).wrapping_mul(0x2545F4914F6CDD1D);
    let pick =
        |state: &mut u64| FILLER_WORDS[(next_u64(state) % FILLER_WORDS.len() as u64) as usize];
    let repo = format!("organic/{}", (b'a' + (i / 40 % 26) as u8) as char);
    let (path, flavor) = match i % 3 {
        0 => (format!("src/unit_{i:03}.rs"), 0),
        1 => (format!("core/part_{i:03}.py"), 1),
        _ => (format!("app/view_{i:03}.js"), 2),
    };
    let mut lines = Vec::new();
    for l in 0..10 {
        let a = pick(&mut state);
        let b = pick(&mut state);
        let c = pick(&mut state);
        lines.push(match flavor {
            0 => format!("let {a}_{i}_{l} = {b}::{c}_{l}(&state_{i});"),
            1 => format!("{a}_{i}_{l} = {b}.{c}_{l}(state_{i})"),
            _ => format!("export const {a}_{i}_{l} = {b}({c}_{i}, {l});"),
        });
    }
    lines.push(match flavor {
        0 => format!("const panel_state_{i:04}: u64 = render_view({i});"),
        1 => format!("PANEL_STATE_{i:04} = dispatch_event({i})"),
        _ => format!("const summarize_row_{i:04} = render_view({i});"),
    });
    lines.push(match flavor {
        0 => format!("fn dispatch_event_{i:04}() -> u32 {{ {i} }}"),
        1 => format!("def summarize_row_{i:04}(x): return x + {i}"),
        _ => format!("function panel_state_{i:04}() {{ return {i}; }}"),
    });
    (repo, path, lines.join("\n").into_bytes())
}

pub fn corpus_docs() -> Vec<CorpusDoc> {
    let mut docs: Vec<CorpusDoc> = DISTINCT
        .iter()
        .map(|(repo, path, content)| {
            (
                repo.to_string(),
                path.to_string(),
                content.as_bytes().to_vec(),
            )
        })
        .collect();
    for i in 0..CORPUS_SIZE - DISTINCT.len() {
        docs.push(synthetic_doc(i));
    }
    docs
}

const QUERIES: &[(&str, &str, QueryKind)] = &[
    ("q01", "retry_with_backoff", QueryKind::Literal),
    ("q02", "lru_cache_insert", QueryKind::Literal),
    ("q03", "parse_json_value", QueryKind::Literal),
    ("q04", "debounce", QueryKind::Literal),
    ("q05", "acquire_token", QueryKind::Literal),
    ("q06", "bloom_filter_add", QueryKind::Literal),
    ("q07", "slugify_title", QueryKind::Literal),
    ("q08", "circuit_breaker_trip", QueryKind::Literal),
    ("q09", "pool_acquire", QueryKind::Literal),
    ("q10", "try_acquire_permit", QueryKind::Literal),
    ("q11", "redact_secrets", QueryKind::Literal),
    ("q12", "throttle", QueryKind::Literal),
    ("q13", "uuid_v4", QueryKind::Literal),
    ("q14", "tail_lines", QueryKind::Literal),
    ("q15", "connection pool", QueryKind::Phrase),
    ("q16", "exponential backoff", QueryKind::Phrase),
    ("q17", "circuit breaker", QueryKind::Phrase),
    ("q18", "least recently used", QueryKind::Phrase),
    ("q19", "rate limiting", QueryKind::Phrase),
    ("q20", "breadth first", QueryKind::Phrase),
    ("q21", "shortest path", QueryKind::Phrase),
    ("q22", "deep merge", QueryKind::Phrase),
    ("q23", "base64", QueryKind::Literal),
    ("q24", "health check", QueryKind::Phrase),
    ("q25", "cache eviction", QueryKind::Phrase),
    ("q26", "unified diff", QueryKind::Phrase),
    ("q27", "consistent hashing", QueryKind::Phrase),
    ("q28", "glob pattern", QueryKind::Phrase),
    ("q29", r"retry_with_\w+", QueryKind::Regex),
];

const QRELS: &[(&str, &[(&str, u8)])] = &[
    (
        "q01",
        &[("netkit/src/retry.rs", 3), ("netkit/src/backoff.rs", 1)],
    ),
    ("q02", &[("cachekit/src/lru_cache.rs", 3)]),
    (
        "q03",
        &[
            ("jskit/src/jsonParse.js", 3),
            ("pyutil/src/json_safe.py", 2),
            ("infra/src/config_loader.rs", 1),
        ],
    ),
    (
        "q04",
        &[
            ("jskit/src/debounce.js", 3),
            ("netkit/src/debounce.rs", 2),
            ("jskit/src/throttle.js", 1),
        ],
    ),
    (
        "q05",
        &[
            ("netkit/src/rate_limiter.rs", 3),
            ("pyutil/src/ratelimit.py", 2),
        ],
    ),
    (
        "q06",
        &[
            ("algos/src/bloom_filter.rs", 3),
            ("datakit/src/bloom_filter.py", 2),
        ],
    ),
    (
        "q07",
        &[("pyutil/src/slugify.py", 3), ("strings/src/slugify.rs", 2)],
    ),
    ("q08", &[("netkit/src/circuit_breaker.rs", 3)]),
    (
        "q09",
        &[
            ("netkit/src/connection_pool.rs", 3),
            ("jskit/src/connectionPool.js", 2),
        ],
    ),
    ("q10", &[("infra/src/semaphore.rs", 3)]),
    (
        "q11",
        &[("pyutil/src/redact.py", 3), ("infra/src/tracer.rs", 1)],
    ),
    (
        "q12",
        &[
            ("jskit/src/throttle.js", 3),
            ("jskit/src/debounce.js", 1),
            ("netkit/src/rate_limiter.rs", 1),
        ],
    ),
    (
        "q13",
        &[("infra/src/uuid.rs", 3), ("datakit/src/ids.py", 2)],
    ),
    (
        "q14",
        &[("pyutil/src/tail.py", 3), ("datakit/src/reader.py", 2)],
    ),
    (
        "q15",
        &[
            ("netkit/src/connection_pool.rs", 3),
            ("jskit/src/connectionPool.js", 2),
            ("netkit/src/health_check.rs", 1),
            ("infra/src/config_loader.rs", 1),
        ],
    ),
    (
        "q16",
        &[
            ("netkit/src/backoff.rs", 3),
            ("netkit/src/retry.rs", 2),
            ("pyutil/src/retry.py", 2),
        ],
    ),
    (
        "q17",
        &[
            ("netkit/src/circuit_breaker.rs", 3),
            ("netkit/src/health_check.rs", 1),
        ],
    ),
    (
        "q18",
        &[
            ("cachekit/src/lru_cache.rs", 3),
            ("jskit/src/lruCache.js", 2),
        ],
    ),
    (
        "q19",
        &[
            ("netkit/src/rate_limiter.rs", 3),
            ("pyutil/src/ratelimit.py", 2),
            ("infra/src/config_loader.rs", 1),
        ],
    ),
    (
        "q20",
        &[("algos/src/bfs.rs", 3), ("algos/src/dijkstra.rs", 1)],
    ),
    (
        "q21",
        &[("algos/src/dijkstra.rs", 3), ("algos/src/bfs.rs", 1)],
    ),
    (
        "q22",
        &[
            ("datakit/src/deep_merge.py", 3),
            ("pyutil/src/deepmerge.py", 2),
            ("infra/src/config_loader.rs", 1),
        ],
    ),
    (
        "q23",
        &[
            ("strings/src/base64.rs", 3),
            ("datakit/src/encoding.py", 2),
            ("pyutil/src/redact.py", 1),
        ],
    ),
    (
        "q24",
        &[
            ("netkit/src/health_check.rs", 3),
            ("netkit/src/connection_pool.rs", 1),
        ],
    ),
    (
        "q25",
        &[
            ("cachekit/src/eviction.rs", 3),
            ("cachekit/src/lru_cache.rs", 2),
            ("jskit/src/lruCache.js", 1),
        ],
    ),
    ("q26", &[("algos/src/diff.rs", 3)]),
    (
        "q27",
        &[
            ("infra/src/consistent_hash.rs", 3),
            ("infra/src/worker_queue.rs", 1),
        ],
    ),
    (
        "q28",
        &[("infra/src/glob.rs", 3), ("infra/src/worker_queue.rs", 1)],
    ),
    (
        "q29",
        &[
            ("netkit/src/retry.rs", 3),
            ("pyutil/src/retry.py", 2),
            ("netkit/src/backoff.rs", 1),
        ],
    ),
];

pub fn golden_set() -> GoldenSet {
    let queries = QUERIES
        .iter()
        .map(|(id, text, kind)| Query {
            id,
            text,
            kind: *kind,
        })
        .collect();
    let mut qrels = Qrels::new();
    for (qid, docs) in QRELS {
        let entry = qrels.entry(qid).or_default();
        for (doc, gain) in *docs {
            entry.insert(doc, *gain);
        }
    }
    GoldenSet { queries, qrels }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn corpus_has_exactly_300_docs() {
        let docs = corpus_docs();
        assert_eq!(docs.len(), CORPUS_SIZE);
        assert_eq!(DISTINCT.len(), 39);
    }

    #[test]
    fn corpus_paths_are_unique() {
        let docs = corpus_docs();
        let keys: HashSet<(&str, &str)> = docs
            .iter()
            .map(|(r, p, _)| (r.as_str(), p.as_str()))
            .collect();
        assert_eq!(keys.len(), docs.len());
    }

    #[test]
    fn corpus_is_deterministic() {
        assert_eq!(corpus_docs(), corpus_docs());
    }

    #[test]
    fn corpus_files_are_utf8_and_small() {
        for (_, _, bytes) in corpus_docs() {
            assert!(std::str::from_utf8(&bytes).is_ok());
            assert!(bytes.len() < 100_000);
        }
    }

    #[test]
    fn golden_set_shape() {
        let g = golden_set();
        assert_eq!(g.queries.len(), 29);
        assert_eq!(
            g.queries
                .iter()
                .filter(|q| q.kind == QueryKind::Regex)
                .count(),
            1
        );
        assert_eq!(g.qrels.len(), g.queries.len());
        for q in &g.queries {
            assert!(g.qrels.contains_key(q.id), "missing qrels for {}", q.id);
            assert!(!g.qrels[q.id].is_empty());
            assert!(g.qrels[q.id].values().all(|v| *v <= 3));
        }
    }

    #[test]
    fn qrels_reference_existing_docs() {
        let docs = corpus_docs();
        let keys: HashSet<String> = docs.iter().map(|(r, p, _)| format!("{r}/{p}")).collect();
        let g = golden_set();
        for (qid, judged) in &g.qrels {
            for doc in judged.keys() {
                assert!(keys.contains(*doc), "{qid} judges unknown doc {doc}");
            }
        }
    }

    #[test]
    fn every_literal_query_matches_its_judged_docs() {
        let docs = corpus_docs();
        let g = golden_set();
        for q in &g.queries {
            if q.kind == QueryKind::Regex {
                continue;
            }
            for doc in g.qrels[q.id].keys() {
                let hit = docs.iter().any(|(r, p, bytes)| {
                    format!("{r}/{p}") == *doc
                        && String::from_utf8_lossy(bytes)
                            .to_lowercase()
                            .contains(&q.text.to_lowercase())
                });
                assert!(hit, "{}: needle {:?} absent from {doc}", q.id, q.text);
            }
        }
    }

    #[test]
    fn filler_never_contains_a_query_needle() {
        let docs = corpus_docs();
        let g = golden_set();
        let distinct_keys: HashSet<String> = DISTINCT
            .iter()
            .map(|(r, p, _)| format!("{r}/{p}"))
            .collect();
        for (repo, path, bytes) in &docs {
            let key = format!("{repo}/{path}");
            if distinct_keys.contains(&key) {
                continue;
            }
            let lower = String::from_utf8_lossy(bytes).to_lowercase();
            for q in &g.queries {
                if q.kind != QueryKind::Regex {
                    assert!(
                        !lower.contains(&q.text.to_lowercase()),
                        "filler {key} contains needle {:?}",
                        q.text
                    );
                }
            }
        }
    }

    #[test]
    fn needle_spot_checks() {
        let docs = corpus_docs();
        let find = |needle: &str| {
            docs.iter()
                .filter(|(r, p, b)| {
                    (format!("{r}/{p}").contains(needle))
                        || String::from_utf8_lossy(b).contains(needle)
                })
                .count()
        };
        assert!(find("retry_with_backoff") >= 2);
        assert!(find("circuit_breaker_trip") >= 1);
        assert!(find("exponential backoff") >= 3);
        assert!(find("least recently used") >= 2);
    }
}
