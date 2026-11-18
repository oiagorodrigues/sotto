use std::{
    collections::{HashMap, VecDeque},
    sync::Mutex,
    time::{Duration, Instant},
};

/// In-memory sliding-window limiter. State is per process: it resets on restart and is not
/// shared between instances (the realtime token limit uses the database for that reason).
#[derive(Debug, Default)]
pub struct RateLimiter {
    hits: Mutex<HashMap<String, VecDeque<Instant>>>,
}

const PRUNE_ABOVE: usize = 10_000;
const LONGEST_WINDOW: Duration = Duration::from_secs(3600);

impl RateLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a hit on `key`. `Err` carries the seconds until the oldest hit leaves the window.
    pub fn check(&self, key: &str, max: u32, window: Duration) -> Result<(), u64> {
        self.check_at(key, max, window, Instant::now())
    }

    fn check_at(&self, key: &str, max: u32, window: Duration, now: Instant) -> Result<(), u64> {
        let mut map = self.hits.lock().unwrap_or_else(|e| e.into_inner());
        if map.len() > PRUNE_ABOVE {
            map.retain(|_, q| {
                q.back()
                    .is_some_and(|t| now.duration_since(*t) < LONGEST_WINDOW)
            });
        }
        let q = map.entry(key.to_owned()).or_default();
        while q.front().is_some_and(|t| now.duration_since(*t) >= window) {
            q.pop_front();
        }
        if q.len() >= max as usize {
            let wait = q
                .front()
                .map(|t| window.saturating_sub(now.duration_since(*t)))
                .unwrap_or(window);
            return Err(wait.as_secs() + 1);
        }
        q.push_back(now);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_after_max_and_recovers() {
        let rl = RateLimiter::new();
        let w = Duration::from_secs(60);
        let t0 = Instant::now();
        for _ in 0..3 {
            assert!(rl.check_at("k", 3, w, t0).is_ok());
        }
        assert!(rl.check_at("k", 3, w, t0).is_err());
        assert!(rl.check_at("other", 3, w, t0).is_ok());
        let retry = rl
            .check_at("k", 3, w, t0 + Duration::from_secs(10))
            .unwrap_err();
        assert!((50..=51).contains(&retry));
        assert!(rl.check_at("k", 3, w, t0 + w).is_ok());
    }
}
