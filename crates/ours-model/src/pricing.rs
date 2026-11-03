use serde::{Deserialize, Serialize};

use crate::Usage;

/// Dollars per million tokens.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Pricing {
    pub input_per_million: f64,
    pub output_per_million: f64,
    pub cached_input_per_million: f64,
}

impl Pricing {
    /// The cost of one call in dollars. Cached tokens are charged at the
    /// cached rate and the rest of the prompt at the input rate.
    pub fn cost(&self, usage: &Usage) -> f64 {
        let cached = usage.cached_input_tokens.min(usage.input_tokens);
        let fresh = usage.input_tokens - cached;
        (fresh as f64 * self.input_per_million
            + cached as f64 * self.cached_input_per_million
            + usage.output_tokens as f64 * self.output_per_million)
            / 1_000_000.0
    }

    pub(crate) fn is_valid(&self) -> bool {
        [
            self.input_per_million,
            self.output_per_million,
            self.cached_input_per_million,
        ]
        .iter()
        .all(|rate| rate.is_finite() && *rate >= 0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pricing() -> Pricing {
        Pricing {
            input_per_million: 3.0,
            output_per_million: 15.0,
            cached_input_per_million: 0.3,
        }
    }

    #[test]
    fn splits_cached_from_fresh_input() {
        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 100_000,
            cached_input_tokens: 800_000,
        };
        // 0.2M fresh at 3.0, 0.8M cached at 0.3, 0.1M output at 15.0.
        let cost = pricing().cost(&usage);
        assert!((cost - (0.6 + 0.24 + 1.5)).abs() < 1e-9, "{cost}");
    }

    #[test]
    fn cached_above_input_does_not_underflow() {
        let usage = Usage {
            input_tokens: 10,
            output_tokens: 0,
            cached_input_tokens: 50,
        };
        assert!(pricing().cost(&usage) < 1e-4);
    }

    #[test]
    fn zero_usage_costs_nothing() {
        assert_eq!(pricing().cost(&Usage::default()), 0.0);
    }

    #[test]
    fn rejects_negative_and_non_finite_rates() {
        let mut p = pricing();
        assert!(p.is_valid());
        p.output_per_million = -1.0;
        assert!(!p.is_valid());
        p.output_per_million = f64::NAN;
        assert!(!p.is_valid());
    }
}
