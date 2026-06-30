//! TCP serial-number arithmetic (RFC 1982). The single most error-prone area (design §4).

/// A TCP sequence or acknowledgement number. Wraps mod 2^32 under RFC 1982 comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TcpSeq(pub u32);

const HALF: u32 = 0x8000_0000;

impl TcpSeq {
    /// Returns `true` if `self` precedes `other` in RFC 1982 serial order.
    #[must_use]
    pub fn serial_lt(self, other: TcpSeq) -> bool {
        let forward = other.0.wrapping_sub(self.0);
        forward != 0 && forward < HALF
    }

    /// Returns `true` if `self` follows `other` in RFC 1982 serial order.
    #[must_use]
    pub fn serial_gt(self, other: TcpSeq) -> bool {
        other.serial_lt(self)
    }

    /// Returns the forward distance (bytes advanced, wrapping) from `earlier` to `self`.
    #[must_use]
    pub fn serial_diff(self, earlier: TcpSeq) -> u32 {
        self.0.wrapping_sub(earlier.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn forward_small_step_is_less() {
        assert!(TcpSeq(10).serial_lt(TcpSeq(20)));
        assert!(TcpSeq(20).serial_gt(TcpSeq(10)));
    }

    #[test]
    fn wrap_forward_is_still_less_not_greater() {
        // near u32::MAX, +16 wraps forward — must read as advance, not a new instance.
        let a = TcpSeq(u32::MAX - 5);
        let b = TcpSeq(10); // a + 16 (wrapped)
        assert!(a.serial_lt(b));
        assert!(!a.serial_gt(b));
        assert_eq!(b.serial_diff(a), 16);
    }

    #[test]
    fn irreflexive() {
        assert!(!TcpSeq(42).serial_lt(TcpSeq(42)));
        assert!(!TcpSeq(42).serial_gt(TcpSeq(42)));
    }

    proptest! {
        #[test]
        fn forward_distance_under_half_is_lt(base in any::<u32>(), d in 1u32..HALF) {
            let a = TcpSeq(base);
            let b = TcpSeq(base.wrapping_add(d));
            prop_assert!(a.serial_lt(b));
            prop_assert!(b.serial_gt(a));
            prop_assert_eq!(b.serial_diff(a), d);
        }

        #[test]
        fn antisymmetric_off_half(a in any::<u32>(), b in any::<u32>()) {
            let (x, y) = (TcpSeq(a), TcpSeq(b));
            let d = b.wrapping_sub(a);
            prop_assume!(a != b && d != HALF); // exact half is intentionally undefined
            prop_assert_ne!(x.serial_lt(y), y.serial_lt(x));
        }
    }
}
