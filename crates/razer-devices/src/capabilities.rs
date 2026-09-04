// SPDX-License-Identifier: GPL-2.0-or-later
//! Hand-rolled bitflags describing which commands a device supports.
//!
//! No `bitflags` crate dependency — kept dependency-free per the frozen API.

/// A bitset of the commands a [`crate::table::DeviceEntry`] supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities(pub u32);

impl Capabilities {
    /// No capabilities set.
    pub const NONE: Self = Capabilities(0);
    /// `razer_chroma_standard_set/get_device_mode` — razerchromacommon.c:25-54.
    pub const DEVICE_MODE: Self = Capabilities(1 << 0);
    /// `razer_chroma_standard_get_firmware_version` — razerchromacommon.c:67-70.
    pub const FIRMWARE_VERSION: Self = Capabilities(1 << 1);
    /// `razer_chroma_standard_get_serial` — razerchromacommon.c:59-62.
    pub const SERIAL: Self = Capabilities(1 << 2);
    /// `razer_chroma_extended_matrix_brightness` set/get — razerchromacommon.c:714-738.
    pub const BRIGHTNESS: Self = Capabilities(1 << 3);
    /// `razer_chroma_extended_matrix_effect_static` — razerchromacommon.c:481-520.
    pub const STATIC_EFFECT: Self = Capabilities(1 << 4);
    /// Legacy or v2 poll-rate set/get — razerchromacommon.c:1092-1189.
    pub const POLL_RATE: Self = Capabilities(1 << 5);

    /// Returns `true` if `self` contains every flag set in `other`.
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Returns the union of `self` and `other`.
    pub const fn union(self, other: Self) -> Self {
        Capabilities(self.0 | other.0)
    }

    /// Returns the raw bitmask.
    pub const fn bits(self) -> u32 {
        self.0
    }
}

impl core::ops::BitOr for Capabilities {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_contains_nothing() {
        assert!(!Capabilities::NONE.contains(Capabilities::SERIAL));
    }

    #[test]
    fn union_contains_members_not_others() {
        let c = Capabilities::SERIAL | Capabilities::BRIGHTNESS;
        assert!(c.contains(Capabilities::SERIAL));
        assert!(c.contains(Capabilities::BRIGHTNESS));
        assert!(!c.contains(Capabilities::POLL_RATE));
    }

    #[test]
    fn all_flags_distinct_powers_of_two() {
        let flags = [
            Capabilities::DEVICE_MODE,
            Capabilities::FIRMWARE_VERSION,
            Capabilities::SERIAL,
            Capabilities::BRIGHTNESS,
            Capabilities::STATIC_EFFECT,
            Capabilities::POLL_RATE,
        ];
        for f in flags {
            assert_eq!(
                f.bits().count_ones(),
                1,
                "{:#x} is not a power of two",
                f.bits()
            );
        }
        for i in 0..flags.len() {
            for j in (i + 1)..flags.len() {
                assert_ne!(flags[i].bits(), flags[j].bits());
            }
        }
    }
}
