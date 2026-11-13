//! Cheap syntax-only work estimates; no type inference or cold-body lowering.
use std::time::Duration;
#[derive(Debug, Clone, Copy)]
pub struct EagerPolicy {
    pub enabled: bool,
    pub min_words: usize,
    pub max_words: usize,
    pub max_routines: usize,
    pub max_total_words: usize,
    pub predicted_budget: Duration,
    pub forced_max_words: usize,
}
impl Default for EagerPolicy {
    fn default() -> Self {
        Self {
            enabled: std::env::var("CAIE_EAGER_ROUTINES").as_deref() != Ok("0"),
            min_words: 16,
            max_words: 128,
            max_routines: 4,
            max_total_words: 256,
            predicted_budget: Duration::from_millis(2),
            forced_max_words: 4096,
        }
    }
}
impl EagerPolicy {
    pub fn size_reason(self, words: usize, hint: cyallo_ast::CompileHint) -> Option<&'static str> {
        use cyallo_ast::CompileHint::*;
        if hint == AlwaysLazy || hint == Lazy {
            return Some("source policy forbids proactive compilation");
        }
        if !self.enabled {
            return Some("eager scheduling disabled");
        }
        if hint == AlwaysEager {
            return (words > self.forced_max_words)
                .then_some("forced routine exceeds hard eager size limit");
        }
        if words < self.min_words {
            Some("below small threshold; prefer inlining")
        } else if words >= self.max_words {
            Some("at or above medium threshold; retain lazy tiering")
        } else {
            None
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EagerDecision {
    Queued(crate::backend::Tier),
    AlreadyRequested,
    Deferred(&'static str),
}
