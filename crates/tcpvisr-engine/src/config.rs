//! Engine tuning knobs (design §10.M2, ADR-0006).

use tcpvisr_core::Nanos;

/// Connection-tracker configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineConfig {
    /// Idle gap after which a fresh SYN on the same pair starts a new instance.
    pub dead_after: Nanos,
    /// Minimum backward serial distance that reads as a fresh-ISN reset. Must be `< 2^31`
    /// (no backward serial distance can exceed the midpoint) or the rule is unreachable.
    pub reset_threshold: u32,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            dead_after: Nanos(120_000_000_000),
            reset_threshold: 1 << 30,
        }
    }
}
