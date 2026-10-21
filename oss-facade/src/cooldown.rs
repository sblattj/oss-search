//! Per-backend rate-limit cooldown: once a backend answers "rate limited",
//! further calls are skipped locally until the reset time passes.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::time::Instant;

/// A 429 with no usable hint.
pub const DEFAULT_COOLDOWN: Duration = Duration::from_secs(60);
/// Cap on any wait.
pub const MAX_COOLDOWN: Duration = Duration::from_secs(3600);
/// Retry in-call only if the wait is at most this.
pub const SHORT_RETRY_MAX: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, Default)]
pub struct Cooldown {
    until: Arc<Mutex<Option<Instant>>>,
}

impl Cooldown {
    pub fn new() -> Self {
        Self::default()
    }

    /// Some(remaining) while cooling down; None when not (clears expired state).
    pub fn remaining(&self) -> Option<Duration> {
        let mut guard = self.until.lock().unwrap_or_else(|e| e.into_inner());
        let until = (*guard)?;
        let now = Instant::now();
        if until > now {
            Some(until - now)
        } else {
            *guard = None;
            None
        }
    }

    /// Start or extend the cooldown. Never shortens a longer existing one.
    pub fn trip(&self, retry_after: Option<Duration>) {
        let wait = retry_after.unwrap_or(DEFAULT_COOLDOWN).min(MAX_COOLDOWN);
        if wait.is_zero() {
            return;
        }
        let new_until = Instant::now() + wait;
        let mut guard = self.until.lock().unwrap_or_else(|e| e.into_inner());
        match *guard {
            Some(existing) if existing >= new_until => {}
            _ => *guard = Some(new_until),
        }
    }
}

/// "45s", "12m 30s", "1h 0m" (whole seconds, sub-second rounds up to 1s).
pub fn format_wait(d: Duration) -> String {
    let mut secs = d.as_secs();
    if d.subsec_nanos() > 0 {
        secs += 1;
    }
    if secs >= 3600 {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

pub fn skipped_reason(backend: &str, remaining: Duration) -> String {
    format!(
        "{backend} rate limited; resets in {}; not called (cooling down after an earlier rate-limit response)",
        format_wait(remaining)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(got: Option<Duration>, want: Duration) -> bool {
        got.is_some_and(|g| g <= want && g + Duration::from_secs(1) >= want)
    }

    #[tokio::test(start_paused = true)]
    async fn trip_then_expire() {
        let c = Cooldown::new();
        assert_eq!(c.remaining(), None);
        c.trip(Some(Duration::from_secs(60)));
        assert!(near(c.remaining(), Duration::from_secs(60)));
        tokio::time::advance(Duration::from_secs(61)).await;
        assert_eq!(c.remaining(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn default_and_cap() {
        let c = Cooldown::new();
        c.trip(None);
        assert!(near(c.remaining(), DEFAULT_COOLDOWN));
        let c = Cooldown::new();
        c.trip(Some(Duration::from_secs(7200)));
        assert!(near(c.remaining(), MAX_COOLDOWN));
    }

    #[tokio::test(start_paused = true)]
    async fn never_shortens() {
        let c = Cooldown::new();
        c.trip(Some(Duration::from_secs(600)));
        c.trip(Some(Duration::from_secs(5)));
        assert!(near(c.remaining(), Duration::from_secs(600)));
    }

    #[tokio::test(start_paused = true)]
    async fn zero_wait_trips_nothing() {
        let c = Cooldown::new();
        c.trip(Some(Duration::ZERO));
        assert_eq!(c.remaining(), None);
    }

    #[test]
    fn formatting() {
        assert_eq!(format_wait(Duration::from_secs(45)), "45s");
        assert_eq!(format_wait(Duration::from_secs(750)), "12m 30s");
        assert_eq!(format_wait(Duration::from_secs(3600)), "1h 0m");
        assert_eq!(format_wait(Duration::from_millis(300)), "1s");
        assert_eq!(
            skipped_reason("github code search", Duration::from_secs(750)),
            "github code search rate limited; resets in 12m 30s; not called (cooling down after an earlier rate-limit response)"
        );
    }
}
