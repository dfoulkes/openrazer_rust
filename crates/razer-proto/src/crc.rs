// SPDX-License-Identifier: GPL-2.0-or-later
//! The Razer report checksum.
//!
//! Faithful port of `razer_calculate_crc()`, `razercommon.c:110-122`:
//!
//! ```c
//! unsigned char crc = 0;
//! unsigned char *_report = (unsigned char*)report;
//! for(i = 2; i < 88; i++) {
//!     crc ^= _report[i];
//! }
//! ```
//!
//! Plain XOR accumulate, seed 0, no table, no reflection, no final XOR.
//!
//! The covered range is byte index 2 through byte index 87 **inclusive** (86
//! bytes). Deliberately excluded: byte 0 (`status`), byte 1 (`transaction id`),
//! byte 88 (the checksum itself) and byte 89 (`reserved`).
//!
//! Because byte 1 sits outside the covered range, changing the transaction id
//! does **not** invalidate the checksum. That is load-bearing: the transport may
//! stamp the id before or after computing the checksum, and — less happily — a
//! packet carrying a bogus transaction id is still checksum-valid, which is
//! exactly why nothing upstream catches the `0xFF` bug this project exists to fix.

use crate::report::REPORT_LEN;

/// XOR of `bytes[2..88]` (indices 2..=87 inclusive), seed 0.
///
/// ```
/// # use razer_proto::crc;
/// assert_eq!(crc(&[0u8; 90]), 0x00);
/// ```
pub fn crc(bytes: &[u8; REPORT_LEN]) -> u8 {
    bytes[2..88].iter().fold(0u8, |acc, b| acc ^ b)
}

/// True iff the checksum byte at index 88 matches [`crc`].
pub fn verify_crc(bytes: &[u8; REPORT_LEN]) -> bool {
    bytes[88] == crc(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Acceptance criterion 10.
    #[test]
    fn crc_of_all_zeros_is_zero() {
        assert_eq!(crc(&[0u8; REPORT_LEN]), 0x00);
    }

    /// Acceptance criterion 11: status and transaction id are outside the range.
    #[test]
    fn status_and_transaction_id_do_not_affect_crc() {
        let mut bytes = [0u8; REPORT_LEN];
        // Give the covered range something non-trivial to chew on.
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(7);
        }
        let baseline = crc(&bytes);

        for v in 0..=u8::MAX {
            bytes[0] = v;
            assert_eq!(crc(&bytes), baseline, "status {v:#04x} changed the crc");
        }
        bytes[0] = 0;

        for v in 0..=u8::MAX {
            bytes[1] = v;
            assert_eq!(
                crc(&bytes),
                baseline,
                "transaction id {v:#04x} changed the crc"
            );
        }
    }

    /// Acceptance criterion 12: the checksum byte and the reserved byte are excluded.
    #[test]
    fn crc_and_reserved_bytes_do_not_affect_crc() {
        let mut bytes = [0u8; REPORT_LEN];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(3);
        }
        let baseline = crc(&bytes);

        for v in 0..=u8::MAX {
            bytes[88] = v;
            assert_eq!(crc(&bytes), baseline);
        }
        bytes[88] = 0;

        for v in 0..=u8::MAX {
            bytes[89] = v;
            assert_eq!(crc(&bytes), baseline);
        }
    }

    /// Acceptance criterion 13: every byte in 2..=87 is covered.
    #[test]
    fn every_covered_byte_changes_the_crc() {
        let bytes = [0u8; REPORT_LEN];
        let baseline = crc(&bytes);
        for i in 2..=87usize {
            let mut flipped = bytes;
            flipped[i] ^= 0x01;
            assert_ne!(
                crc(&flipped),
                baseline,
                "byte {i} is not covered by the crc"
            );
        }
    }

    #[test]
    fn verify_crc_round_trips() {
        let mut bytes = [0u8; REPORT_LEN];
        bytes[5] = 0x02;
        bytes[7] = 0x04;
        bytes[8] = 0x03;
        bytes[88] = crc(&bytes);
        assert!(verify_crc(&bytes));
        bytes[88] ^= 0xFF;
        assert!(!verify_crc(&bytes));
    }
}
