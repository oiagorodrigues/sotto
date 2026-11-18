const RATE_WINDOW_MS: u64 = 60_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuardConfig {
    pub cooldown_ms: u64,
    pub max_starts_per_minute: u32,
    pub spend_cap_micro_usd: u64,
}

impl Default for GuardConfig {
    fn default() -> Self {
        Self {
            cooldown_ms: 8_000,
            max_starts_per_minute: 6,
            spend_cap_micro_usd: 500_000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardBlock {
    Cooldown { remaining_ms: u64 },
    RateLimited,
    SpendCap,
}

/// Limits on automatic answers (ADR 0006). Pure logic over the times passed in.
#[derive(Debug, Clone)]
pub struct Guards {
    cfg: GuardConfig,
    starts: Vec<u64>,
    last_reveal_ms: Option<u64>,
    spent_micro_usd: u64,
}

impl Guards {
    #[must_use]
    pub fn new(cfg: GuardConfig) -> Self {
        Self {
            cfg,
            starts: Vec::new(),
            last_reveal_ms: None,
            spent_micro_usd: 0,
        }
    }

    /// The spend cap wins over the cooldown, which wins over the rate limit, because only the
    /// cap is permanent for the session.
    pub fn check(&self, now_ms: u64) -> Result<(), GuardBlock> {
        if self.spent_micro_usd >= self.cfg.spend_cap_micro_usd {
            return Err(GuardBlock::SpendCap);
        }
        if let Some(last) = self.last_reveal_ms {
            let elapsed = now_ms.saturating_sub(last);
            if elapsed < self.cfg.cooldown_ms {
                return Err(GuardBlock::Cooldown {
                    remaining_ms: self.cfg.cooldown_ms - elapsed,
                });
            }
        }
        let window_start = now_ms.saturating_sub(RATE_WINDOW_MS);
        let recent = self.starts.iter().filter(|&&t| t > window_start).count();
        if recent >= self.cfg.max_starts_per_minute as usize {
            return Err(GuardBlock::RateLimited);
        }
        Ok(())
    }

    pub fn record_start(&mut self, now_ms: u64) {
        let window_start = now_ms.saturating_sub(RATE_WINDOW_MS);
        self.starts.retain(|&t| t > window_start);
        self.starts.push(now_ms);
    }

    pub fn record_reveal(&mut self, now_ms: u64) {
        self.last_reveal_ms = Some(now_ms);
    }

    /// Applies to the next `check`, so a lowered cap takes effect for the session in progress.
    pub fn set_spend_cap(&mut self, micro_usd: u64) {
        self.cfg.spend_cap_micro_usd = micro_usd;
    }

    pub fn record_spend(&mut self, micro_usd: u64) {
        self.spent_micro_usd = self.spent_micro_usd.saturating_add(micro_usd);
    }

    #[must_use]
    pub fn spent_micro_usd(&self) -> u64 {
        self.spent_micro_usd
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowering_the_cap_blocks_at_once() {
        let mut g = Guards::new(GuardConfig::default());
        g.record_spend(100_000);
        assert_eq!(g.check(0), Ok(()));
        g.set_spend_cap(100_000);
        assert_eq!(g.check(0), Err(GuardBlock::SpendCap));
        g.set_spend_cap(200_000);
        assert_eq!(g.check(0), Ok(()));
    }

    #[test]
    fn fresh_guards_allow() {
        assert_eq!(Guards::new(GuardConfig::default()).check(0), Ok(()));
    }

    #[test]
    fn cooldown_after_reveal() {
        let mut g = Guards::new(GuardConfig::default());
        g.record_reveal(10_000);
        assert_eq!(
            g.check(12_000),
            Err(GuardBlock::Cooldown {
                remaining_ms: 6_000
            })
        );
        assert_eq!(g.check(18_000), Ok(()));
        assert_eq!(
            g.check(5_000),
            Err(GuardBlock::Cooldown {
                remaining_ms: 8_000
            })
        );
    }

    #[test]
    fn rate_limit_counts_starts_in_the_last_minute() {
        let mut g = Guards::new(GuardConfig::default());
        for i in 0..6 {
            g.record_start(1_000 + i * 100);
        }
        assert_eq!(g.check(30_000), Err(GuardBlock::RateLimited));
        assert_eq!(g.check(61_001), Ok(()));
    }

    #[test]
    fn spend_cap_blocks_and_wins() {
        let mut g = Guards::new(GuardConfig::default());
        g.record_reveal(0);
        g.record_spend(499_999);
        assert!(matches!(g.check(100), Err(GuardBlock::Cooldown { .. })));
        g.record_spend(1);
        assert_eq!(g.check(100), Err(GuardBlock::SpendCap));
        g.record_spend(u64::MAX);
        assert_eq!(g.check(1_000_000), Err(GuardBlock::SpendCap));
    }
}
