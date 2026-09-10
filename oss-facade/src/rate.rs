use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Semaphore;

#[derive(Clone)]
pub struct RateLimiter {
    permits: Arc<Semaphore>,
}

impl RateLimiter {
    pub fn new(max_per_interval: u32, interval: Duration) -> Self {
        let spacing = interval / max_per_interval.max(1);
        let permits = Arc::new(Semaphore::new(0));
        tokio::spawn({
            let permits = Arc::clone(&permits);
            async move {
                let mut ticker = tokio::time::interval(spacing);
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    ticker.tick().await;
                    if permits.available_permits() < 1 {
                        permits.add_permits(1);
                    }
                }
            }
        });
        Self { permits }
    }

    pub fn uncapped() -> Self {
        Self {
            permits: Arc::new(Semaphore::new(i32::MAX as usize)),
        }
    }

    pub async fn acquire(&self) {
        if let Ok(permit) = self.permits.acquire().await {
            permit.forget();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::Instant;

    #[tokio::test(start_paused = true)]
    async fn spaces_permits_evenly() {
        let limiter = RateLimiter::new(1, Duration::from_secs(2));
        let start = Instant::now();
        for _ in 0..5 {
            limiter.acquire().await;
        }
        assert_eq!(start.elapsed(), Duration::from_secs(8));
    }

    #[tokio::test(start_paused = true)]
    async fn ten_per_minute_allows_exactly_ten_in_first_window() {
        let limiter = RateLimiter::new(10, Duration::from_secs(60));
        let start = Instant::now();
        let mut grants = Vec::new();
        for _ in 0..15 {
            limiter.acquire().await;
            grants.push(start.elapsed());
        }
        assert_eq!(
            grants.iter().filter(|t| **t < Duration::from_secs(60)).count(),
            10
        );
        assert!(grants[9] < Duration::from_secs(60));
        assert!(grants[10] >= Duration::from_secs(60));
    }
}
