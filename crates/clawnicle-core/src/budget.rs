/// Per-workflow cost + time budget.
///
/// All three caps are optional. A `None` field means "no cap." If every field
/// is `None`, the budget imposes no constraint.
#[derive(Debug, Clone, Copy, Default)]
pub struct Budget {
    pub max_tokens: Option<u64>,
    pub max_usd_micros: Option<u64>,
    pub max_wallclock_ms: Option<u64>,
}

impl Budget {
    pub const fn unlimited() -> Self {
        Self {
            max_tokens: None,
            max_usd_micros: None,
            max_wallclock_ms: None,
        }
    }

    pub fn with_max_tokens(mut self, n: u64) -> Self {
        self.max_tokens = Some(n);
        self
    }

    pub fn with_max_usd_micros(mut self, n: u64) -> Self {
        self.max_usd_micros = Some(n);
        self
    }

    pub fn with_max_wallclock_ms(mut self, n: u64) -> Self {
        self.max_wallclock_ms = Some(n);
        self
    }
}

/// Running total of what the workflow has consumed this process.
///
/// Budget tracking is per-session, not journaled: a process restart resets
/// usage to zero. Tokens and USD could be reconstructed from LlmCallCompleted
/// events in the journal, but wallclock is inherently process-local.
#[derive(Debug, Clone, Copy, Default)]
pub struct BudgetUsage {
    pub tokens: u64,
    pub usd_micros: u64,
    pub wallclock_ms: u64,
}

impl BudgetUsage {
    pub const fn zero() -> Self {
        Self {
            tokens: 0,
            usd_micros: 0,
            wallclock_ms: 0,
        }
    }

    /// Returns the first field name (tokens/usd/wallclock) that would exceed
    /// the given budget, or None if the usage fits.
    pub fn exceeds(&self, budget: &Budget) -> Option<&'static str> {
        if let Some(cap) = budget.max_tokens
            && self.tokens > cap
        {
            return Some("tokens");
        }
        if let Some(cap) = budget.max_usd_micros
            && self.usd_micros > cap
        {
            return Some("usd");
        }
        if let Some(cap) = budget.max_wallclock_ms
            && self.wallclock_ms > cap
        {
            return Some("wallclock");
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlimited_budget_never_exceeds() {
        let usage = BudgetUsage {
            tokens: u64::MAX,
            usd_micros: u64::MAX,
            wallclock_ms: u64::MAX,
        };
        assert_eq!(usage.exceeds(&Budget::unlimited()), None);
    }

    #[test]
    fn wallclock_cap_trips() {
        let usage = BudgetUsage {
            wallclock_ms: 1001,
            ..BudgetUsage::zero()
        };
        let budget = Budget::unlimited().with_max_wallclock_ms(1000);
        assert_eq!(usage.exceeds(&budget), Some("wallclock"));
    }

    #[test]
    fn token_cap_trips_first() {
        let usage = BudgetUsage {
            tokens: 101,
            usd_micros: 100,
            wallclock_ms: 100,
        };
        let budget = Budget {
            max_tokens: Some(100),
            max_usd_micros: Some(200),
            max_wallclock_ms: Some(200),
        };
        assert_eq!(usage.exceeds(&budget), Some("tokens"));
    }
}
