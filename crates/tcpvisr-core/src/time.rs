//! Capture-relative time in nanoseconds (design §4.1).

use core::fmt;

/// Nanoseconds since the capture's first packet record (design §4.1, M1 spec).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Nanos(pub u64);

impl fmt::Display for Nanos {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (s, ns) = (self.0 / 1_000_000_000, self.0 % 1_000_000_000);
        write!(f, "{s}.{ns:09}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_is_seconds_and_padded_nanos() {
        assert_eq!(Nanos(1_500_000_000).to_string(), "1.500000000s");
        assert_eq!(Nanos(42).to_string(), "0.000000042s");
    }
}
