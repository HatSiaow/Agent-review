use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};

use time::OffsetDateTime;

#[derive(Debug, Clone, Copy)]
struct RateLimitConfig {
    max_attempts: usize,
    window: time::Duration,
}

impl RateLimitConfig {
    fn load() -> Self {
        let max_attempts = std::env::var("APP_LOGIN_MAX_ATTEMPTS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(5);
        let window_secs = std::env::var("APP_LOGIN_WINDOW_SECS")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(15 * 60);
        Self {
            max_attempts: max_attempts.max(1),
            window: time::Duration::seconds(window_secs.max(60)),
        }
    }
}

fn attempts_state() -> &'static Mutex<HashMap<String, VecDeque<OffsetDateTime>>> {
    static STATE: OnceLock<Mutex<HashMap<String, VecDeque<OffsetDateTime>>>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn config() -> RateLimitConfig {
    static CFG: OnceLock<RateLimitConfig> = OnceLock::new();
    *CFG.get_or_init(RateLimitConfig::load)
}

fn cleanup_attempts(
    attempts: &mut VecDeque<OffsetDateTime>,
    now: OffsetDateTime,
    cfg: RateLimitConfig,
) {
    let cutoff = now - cfg.window;
    while attempts.front().is_some_and(|t| *t < cutoff) {
        let _ = attempts.pop_front();
    }
}

/// Returns `true` when another login attempt may proceed for `email`.
pub fn allow_attempt(email: &str, now: OffsetDateTime) -> bool {
    let key = email.trim().to_ascii_lowercase();
    let cfg = config();
    let Ok(mut guard) = attempts_state().lock() else {
        return false;
    };
    let attempts = guard.entry(key).or_default();
    cleanup_attempts(attempts, now, cfg);
    attempts.len() < cfg.max_attempts
}

/// Registers a failed login attempt for `email`.
pub fn record_failure(email: &str, now: OffsetDateTime) {
    let key = email.trim().to_ascii_lowercase();
    let cfg = config();
    let Ok(mut guard) = attempts_state().lock() else {
        return;
    };
    let attempts = guard.entry(key).or_default();
    cleanup_attempts(attempts, now, cfg);
    attempts.push_back(now);
}

/// Clears failed-attempt state for `email` after a successful login.
pub fn record_success(email: &str) {
    let key = email.trim().to_ascii_lowercase();
    let Ok(mut guard) = attempts_state().lock() else {
        return;
    };
    guard.remove(&key);
}

#[cfg(test)]
pub fn reset_for_tests() {
    if let Ok(mut guard) = attempts_state().lock() {
        guard.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn allows_up_to_limit_then_blocks() {
        reset_for_tests();
        let now = datetime!(2026-04-15 12:00:00 UTC);
        let email = "owner@example.com";

        for _ in 0..5 {
            assert!(allow_attempt(email, now));
            record_failure(email, now);
        }
        assert!(!allow_attempt(email, now));
    }

    #[test]
    fn success_clears_attempt_window() {
        reset_for_tests();
        let now = datetime!(2026-04-15 12:00:00 UTC);
        let email = "owner@example.com";
        record_failure(email, now);
        record_success(email);
        assert!(allow_attempt(email, now));
    }
}
