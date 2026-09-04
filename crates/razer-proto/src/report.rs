// SPDX-License-Identifier: GPL-2.0-or-later
//! The 90-byte `struct razer_report` and its supporting value types.
//!
//! Layout is a byte-for-byte port of `razercommon.h:124-136`, which carries a
//! `static_assert(sizeof(struct razer_report) == 90)`. Every field is a `u8`
//! except `remaining_packets`, which is a `__be16` at a 2-aligned offset, so the
//! C struct has no padding and its in-memory bytes *are* the wire bytes.
//!
//! | Offset | Size | Field |
//! |--------|------|-------|
//! | 0      | 1    | `status` |
//! | 1      | 1    | `transaction_id` |
//! | 2..=3  | 2    | `remaining_packets` (**big endian**) |
//! | 4      | 1    | `protocol_type` (always 0) |
//! | 5      | 1    | `data_size` (must be <= 80) |
//! | 6      | 1    | `command_class` |
//! | 7      | 1    | `command_id` |
//! | 8..=87 | 80   | `arguments` |
//! | 88     | 1    | `crc` |
//! | 89     | 1    | `reserved` (always 0) |

use crate::crc::crc;

/// Total size of an encoded report, in bytes (`razercommon.h:136`).
pub const REPORT_LEN: usize = 90;

/// Size of the `arguments` array (`razercommon.h:131`).
pub const ARGS_LEN: usize = 80;

/// Report status byte (`razercommon.h:79-83`).
///
/// The host always sends `New` (0x00); the rest are device responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// 0x00 — a new command from host to device.
    New,
    /// 0x01 — `RAZER_CMD_BUSY`. Treated as success by the C driver.
    Busy,
    /// 0x02 — `RAZER_CMD_SUCCESSFUL`.
    Successful,
    /// 0x03 — `RAZER_CMD_FAILURE`.
    Failure,
    /// 0x04 — `RAZER_CMD_TIMEOUT` (no response).
    Timeout,
    /// 0x05 — `RAZER_CMD_NOT_SUPPORTED`.
    NotSupported,
    /// Anything else the device might invent.
    Unknown(u8),
}

impl Status {
    /// Decode a status byte. Never fails — unrecognised values become
    /// [`Status::Unknown`].
    pub const fn from_u8(v: u8) -> Self {
        match v {
            0x00 => Self::New,
            0x01 => Self::Busy,
            0x02 => Self::Successful,
            0x03 => Self::Failure,
            0x04 => Self::Timeout,
            0x05 => Self::NotSupported,
            other => Self::Unknown(other),
        }
    }

    /// Encode back to the wire byte.
    pub const fn to_u8(self) -> u8 {
        match self {
            Self::New => 0x00,
            Self::Busy => 0x01,
            Self::Successful => 0x02,
            Self::Failure => 0x03,
            Self::Timeout => 0x04,
            Self::NotSupported => 0x05,
            Self::Unknown(other) => other,
        }
    }

    /// True for [`Status::Successful`] and [`Status::Busy`] only.
    ///
    /// Mirrors `razer_send_payload()` (`razerkbd_driver.c:447-449`): *"Some
    /// commands respond with 'busy' but succeed. Treat it as success."*
    pub const fn is_ok(self) -> bool {
        matches!(self, Self::Successful | Self::Busy)
    }
}

/// A 24-bit colour, matching `struct razer_rgb` (`razercommon.h:85-89`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    /// Red channel.
    pub r: u8,
    /// Green channel.
    pub g: u8,
    /// Blue channel.
    pub b: u8,
}

impl Rgb {
    /// Build a colour from its three channels.
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

/// Whether the device should persist the setting (`razercommon.h:33-34`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Storage {
    /// `NOSTORE` — apply now, do not persist.
    NoStore = 0x00,
    /// `VARSTORE` — persist to the device's variable store.
    VarStore = 0x01,
}

/// LED identifiers (`razercommon.h:37-57`).
///
/// Only the ids used by the two devices in scope are modelled; the upstream
/// list is far longer and adding to it here would be inventing protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LedId {
    /// `ZERO_LED` — the Basilisk V3 Pro's default LED.
    Zero = 0x00,
    /// `SCROLL_WHEEL_LED`.
    ScrollWheel = 0x01,
    /// `LOGO_LED`.
    Logo = 0x04,
    /// `BACKLIGHT_LED` — the BlackWidow V4 Pro's default LED.
    Backlight = 0x05,
}

/// Operating mode (`razerchromacommon.c:25-44`).
///
/// Upstream explicitly blocks mode `0x02` ("some sort of factory test mode. Not
/// recommended to be used") by rewriting it to `0x00`, so it is simply not
/// representable here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DeviceMode {
    /// 0x00 — normal mode; the device generates its own keystrokes.
    Normal = 0x00,
    /// 0x03 — driver mode; the host takes over.
    Driver = 0x03,
}

impl DeviceMode {
    /// Decode a mode byte. Returns `None` for anything upstream would reject,
    /// including the blocked `0x02`.
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0x00 => Some(Self::Normal),
            0x03 => Some(Self::Driver),
            _ => None,
        }
    }

    /// Encode back to the wire byte.
    pub const fn to_u8(self) -> u8 {
        self as u8
    }
}

/// A polling rate, in hertz.
///
/// Two wire encodings exist and they are *not* interchangeable — see
/// [`PollRate::legacy_code`] and [`PollRate::v2_code`]. The BlackWidow V4 Pro
/// uses v2; the Basilisk V3 Pro uses legacy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollRate {
    /// 125 Hz.
    Hz125,
    /// 250 Hz.
    Hz250,
    /// 500 Hz.
    Hz500,
    /// 1000 Hz.
    Hz1000,
    /// 2000 Hz.
    Hz2000,
    /// 4000 Hz.
    Hz4000,
    /// 8000 Hz.
    Hz8000,
}

impl PollRate {
    /// The rate in hertz.
    pub const fn hz(self) -> u16 {
        match self {
            Self::Hz125 => 125,
            Self::Hz250 => 250,
            Self::Hz500 => 500,
            Self::Hz1000 => 1000,
            Self::Hz2000 => 2000,
            Self::Hz4000 => 4000,
            Self::Hz8000 => 8000,
        }
    }

    /// Build from a rate in hertz, or `None` if it is not a rate Razer defines.
    pub const fn from_hz(hz: u16) -> Option<Self> {
        match hz {
            125 => Some(Self::Hz125),
            250 => Some(Self::Hz250),
            500 => Some(Self::Hz500),
            1000 => Some(Self::Hz1000),
            2000 => Some(Self::Hz2000),
            4000 => Some(Self::Hz4000),
            8000 => Some(Self::Hz8000),
            _ => None,
        }
    }

    /// Legacy encoding (`razerchromacommon.c:1104-1136`): 1000 -> 0x01,
    /// 500 -> 0x02, 125 -> 0x08.
    ///
    /// Returns `None` for a rate the legacy command cannot express. Note that
    /// upstream silently falls back to 500 Hz in that case; we refuse instead,
    /// because quietly setting a rate the caller did not ask for is how you end
    /// up debugging the wrong thing at midnight.
    pub const fn legacy_code(self) -> Option<u8> {
        match self {
            Self::Hz1000 => Some(0x01),
            Self::Hz500 => Some(0x02),
            Self::Hz125 => Some(0x08),
            _ => None,
        }
    }

    /// Decode a legacy code byte.
    pub const fn from_legacy_code(code: u8) -> Option<Self> {
        match code {
            0x01 => Some(Self::Hz1000),
            0x02 => Some(Self::Hz500),
            0x08 => Some(Self::Hz125),
            _ => None,
        }
    }

    /// v2 encoding (`razerchromacommon.c:1153-1189`): 8000 -> 0x01,
    /// 4000 -> 0x02, 2000 -> 0x04, 1000 -> 0x08, 500 -> 0x10, 250 -> 0x20,
    /// 125 -> 0x40.
    ///
    /// Total — every rate is expressible.
    pub const fn v2_code(self) -> u8 {
        match self {
            Self::Hz8000 => 0x01,
            Self::Hz4000 => 0x02,
            Self::Hz2000 => 0x04,
            Self::Hz1000 => 0x08,
            Self::Hz500 => 0x10,
            Self::Hz250 => 0x20,
            Self::Hz125 => 0x40,
        }
    }

    /// Decode a v2 code byte.
    pub const fn from_v2_code(code: u8) -> Option<Self> {
        match code {
            0x01 => Some(Self::Hz8000),
            0x02 => Some(Self::Hz4000),
            0x04 => Some(Self::Hz2000),
            0x08 => Some(Self::Hz1000),
            0x10 => Some(Self::Hz500),
            0x20 => Some(Self::Hz250),
            0x40 => Some(Self::Hz125),
            _ => None,
        }
    }
}

/// A Razer report — the single frame both directions of the protocol use.
///
/// `remaining_packets` is held here in native byte order and encoded big endian
/// by [`RazerReport::to_bytes`], matching the `__be16` in the C struct.
///
/// The `transaction_id` field is deliberately *not* set by anything in
/// [`crate::cmd`]. It comes from the per-device table at send time. Hardcoding
/// it is precisely the upstream bug this crate exists to avoid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RazerReport {
    /// Byte 0. Host always sends [`Status::New`].
    pub status: Status,
    /// Byte 1. Opaque; the transport stamps it from the device table.
    pub transaction_id: u8,
    /// Bytes 2..=3, **big endian** on the wire. Always 0 for every command in scope.
    pub remaining_packets: u16,
    /// Byte 4. Always 0.
    pub protocol_type: u8,
    /// Byte 5. Length of the meaningful payload in `arguments`; must be <= 80.
    pub data_size: u8,
    /// Byte 6.
    pub command_class: u8,
    /// Byte 7. Bit 7 set means device-to-host (a "get").
    pub command_id: u8,
    /// Bytes 8..=87. Zero-filled beyond `data_size`.
    pub arguments: [u8; ARGS_LEN],
    /// Byte 88. Ignored by [`RazerReport::to_bytes`], which always recomputes it.
    pub crc: u8,
    /// Byte 89. Always 0.
    pub reserved: u8,
}

impl RazerReport {
    /// A zeroed report with the given class, id and payload length.
    ///
    /// Mirrors `get_razer_report()` (`razercommon.c:126-139`), including the
    /// transaction id of `0x00` — the caller or transport stamps the real one.
    pub fn new(command_class: u8, command_id: u8, data_size: u8) -> Self {
        Self {
            status: Status::New,
            transaction_id: 0x00,
            remaining_packets: 0,
            protocol_type: 0x00,
            data_size,
            command_class,
            command_id,
            arguments: [0u8; ARGS_LEN],
            crc: 0x00,
            reserved: 0x00,
        }
    }

    /// Stamp the transaction id, consuming and returning `self`.
    #[must_use]
    pub fn with_transaction_id(mut self, tid: u8) -> Self {
        self.transaction_id = tid;
        self
    }

    /// Copy `args` into the front of the argument array.
    ///
    /// Does **not** touch `data_size` — the C constructors set that from a
    /// separate literal, and blurring the two would hide protocol detail.
    ///
    /// # Panics
    ///
    /// Panics if `args.len() > ARGS_LEN`. Every call site in this crate passes a
    /// fixed-length literal, so this is a programming error, not a runtime one.
    #[must_use]
    pub fn with_args(mut self, args: &[u8]) -> Self {
        assert!(
            args.len() <= ARGS_LEN,
            "argument slice of {} bytes exceeds the {ARGS_LEN}-byte array",
            args.len()
        );
        self.arguments[..args.len()].copy_from_slice(args);
        self
    }

    /// Encode to the 90-byte wire form, stamping byte 88 with the computed
    /// checksum. The value of `self.crc` is ignored.
    pub fn to_bytes(&self) -> [u8; REPORT_LEN] {
        let mut out = [0u8; REPORT_LEN];
        out[0] = self.status.to_u8();
        out[1] = self.transaction_id;
        out[2..4].copy_from_slice(&self.remaining_packets.to_be_bytes());
        out[4] = self.protocol_type;
        out[5] = self.data_size;
        out[6] = self.command_class;
        out[7] = self.command_id;
        out[8..88].copy_from_slice(&self.arguments);
        out[89] = self.reserved;
        // Must be last: the checksum covers bytes 2..=87, all of which are now set.
        out[88] = crc(&out);
        out
    }

    /// Field-for-field decode. Performs **no** validation, matching the C
    /// driver, which never checks a response checksum.
    ///
    /// Use [`crate::parse::check_response`] and [`crate::verify_crc`] for that.
    pub fn from_bytes(bytes: &[u8; REPORT_LEN]) -> Self {
        let mut arguments = [0u8; ARGS_LEN];
        arguments.copy_from_slice(&bytes[8..88]);
        Self {
            status: Status::from_u8(bytes[0]),
            transaction_id: bytes[1],
            remaining_packets: u16::from_be_bytes([bytes[2], bytes[3]]),
            protocol_type: bytes[4],
            data_size: bytes[5],
            command_class: bytes[6],
            command_id: bytes[7],
            arguments,
            crc: bytes[88],
            reserved: bytes[89],
        }
    }

    /// The meaningful part of the argument array, clamped to [`ARGS_LEN`].
    pub fn args(&self) -> &[u8] {
        let n = if (self.data_size as usize) < ARGS_LEN {
            self.data_size as usize
        } else {
            ARGS_LEN
        };
        &self.arguments[..n]
    }

    /// True if this is a device-to-host command — bit 7 of `command_id`
    /// (`union command_id_union`, `razercommon.h:99-105`).
    pub fn is_get(&self) -> bool {
        self.command_id & 0x80 != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd;

    /// Acceptance criterion 6.
    #[test]
    fn encoded_report_is_ninety_bytes() {
        assert_eq!(RazerReport::new(0x00, 0x04, 0x02).to_bytes().len(), 90);
        assert_eq!(REPORT_LEN, 90);
        assert_eq!(ARGS_LEN, 80);
    }

    fn sample() -> RazerReport {
        let mut r = RazerReport::new(0x0F, 0x84, 0x03).with_transaction_id(0x1F);
        r.remaining_packets = 0x0102;
        r.protocol_type = 0x00;
        r.reserved = 0x00;
        r.arguments[0] = 0x01;
        r.arguments[1] = 0x05;
        r.arguments[79] = 0xAB;
        r
    }

    /// Acceptance criterion 7.
    #[test]
    fn round_trip_preserves_every_field_and_stamps_crc() {
        let r = sample();
        let bytes = r.to_bytes();
        let back = RazerReport::from_bytes(&bytes);

        assert_eq!(back.status, r.status);
        assert_eq!(back.transaction_id, r.transaction_id);
        assert_eq!(back.remaining_packets, r.remaining_packets);
        assert_eq!(back.protocol_type, r.protocol_type);
        assert_eq!(back.data_size, r.data_size);
        assert_eq!(back.command_class, r.command_class);
        assert_eq!(back.command_id, r.command_id);
        assert_eq!(back.arguments, r.arguments);
        assert_eq!(back.reserved, r.reserved);
        assert_eq!(back.crc, crc(&bytes));

        // And the decoded report differs from the original only in `crc`.
        let mut expected = r;
        expected.crc = crc(&bytes);
        assert_eq!(back, expected);
    }

    /// Acceptance criterion 8.
    #[test]
    fn field_offsets_are_exact() {
        let mut r = RazerReport::new(0xC1, 0xC2, 0xC3).with_transaction_id(0xB1);
        r.status = Status::Unknown(0xB0);
        r.remaining_packets = 0xB2B3;
        r.protocol_type = 0xB4;
        r.reserved = 0xDD;
        for (i, a) in r.arguments.iter_mut().enumerate() {
            *a = 0xE0u8.wrapping_add(i as u8);
        }
        let b = r.to_bytes();

        assert_eq!(b[0], 0xB0, "status@0");
        assert_eq!(b[1], 0xB1, "transaction id@1");
        assert_eq!([b[2], b[3]], [0xB2, 0xB3], "remaining_packets@2..4");
        assert_eq!(b[4], 0xB4, "protocol_type@4");
        assert_eq!(b[5], 0xC3, "data_size@5");
        assert_eq!(b[6], 0xC1, "command_class@6");
        assert_eq!(b[7], 0xC2, "command_id@7");
        assert_eq!(&b[8..88], &r.arguments[..], "arguments@8..88");
        assert_eq!(b[88], crc(&b), "crc@88");
        assert_eq!(b[89], 0xDD, "reserved@89");
    }

    /// Acceptance criterion 9: `remaining_packets` is big endian on the wire.
    #[test]
    fn remaining_packets_is_big_endian() {
        let mut r = RazerReport::new(0x00, 0x04, 0x02);
        r.remaining_packets = 0x0102;
        let b = r.to_bytes();
        assert_eq!(b[2], 0x01);
        assert_eq!(b[3], 0x02);
        assert_eq!(RazerReport::from_bytes(&b).remaining_packets, 0x0102);
    }

    /// Acceptance criterion 14 — the anti-regression guard for the whole project.
    #[test]
    fn golden_vector_set_driver_mode_on_the_blackwidow_v4_pro() {
        let bytes = cmd::set_device_mode(DeviceMode::Driver)
            .with_transaction_id(0x1F)
            .to_bytes();

        let mut expected = [0u8; REPORT_LEN];
        expected[0] = 0x00; // status: new command
        expected[1] = 0x1F; // transaction id: from the device table, NOT 0xFF
        expected[2] = 0x00; // remaining_packets hi
        expected[3] = 0x00; // remaining_packets lo
        expected[4] = 0x00; // protocol_type
        expected[5] = 0x02; // data_size
        expected[6] = 0x00; // command_class
        expected[7] = 0x04; // command_id
        expected[8] = 0x03; // arguments[0]: Driver
        expected[9] = 0x00; // arguments[1]: param, always 0
        expected[88] = 0x05; // crc = 0x02 ^ 0x00 ^ 0x04 ^ 0x03
        expected[89] = 0x00; // reserved

        assert_eq!(bytes, expected);
        // Spelled out: the crc is the XOR of the four non-zero bytes in 2..=87,
        // i.e. data_size 0x02, protocol_type 0x00, command_id 0x04, mode 0x03.
        let by_hand = [0x02u8, 0x00, 0x04, 0x03]
            .iter()
            .fold(0u8, |acc, b| acc ^ b);
        assert_eq!(bytes[88], by_hand);
        assert_eq!(by_hand, 0x05);
    }

    #[test]
    fn transaction_id_does_not_disturb_the_encoded_crc() {
        // The whole reason the upstream 0xFF bug is invisible to the device's
        // own integrity check.
        let base = cmd::set_device_mode(DeviceMode::Driver);
        let good = base.with_transaction_id(0x1F).to_bytes();
        let bugged = base.with_transaction_id(0xFF).to_bytes();
        assert_eq!(good[88], bugged[88]);
        assert_ne!(good[1], bugged[1]);
    }

    #[test]
    fn with_args_fills_the_front_of_the_array() {
        let r = RazerReport::new(0x0F, 0x02, 0x09).with_args(&[1, 2, 3]);
        assert_eq!(&r.arguments[..3], &[1, 2, 3]);
        assert!(r.arguments[3..].iter().all(|&b| b == 0));
        assert_eq!(r.data_size, 0x09, "with_args must not touch data_size");
    }

    #[test]
    fn with_args_accepts_a_full_array() {
        let r = RazerReport::new(0, 0, 0).with_args(&[0xAA; ARGS_LEN]);
        assert!(r.arguments.iter().all(|&b| b == 0xAA));
    }

    #[test]
    #[should_panic(expected = "exceeds")]
    fn with_args_panics_when_too_long() {
        let _ = RazerReport::new(0, 0, 0).with_args(&[0u8; ARGS_LEN + 1]);
    }

    #[test]
    fn args_is_clamped_to_data_size_and_to_the_array() {
        let r = RazerReport::new(0x0F, 0x04, 0x03).with_args(&[0x01, 0x05, 0x80, 0xFF]);
        assert_eq!(r.args(), &[0x01, 0x05, 0x80]);

        let mut oversized = r;
        oversized.data_size = 200;
        assert_eq!(oversized.args().len(), ARGS_LEN);
    }

    #[test]
    fn is_get_reads_the_direction_bit() {
        assert!(!RazerReport::new(0x00, 0x04, 0x02).is_get());
        assert!(RazerReport::new(0x00, 0x84, 0x02).is_get());
        assert!(RazerReport::new(0x00, 0xC0, 0x01).is_get());
    }

    #[test]
    fn status_round_trips_including_unknown() {
        for v in 0..=u8::MAX {
            assert_eq!(Status::from_u8(v).to_u8(), v);
        }
        assert_eq!(Status::from_u8(0x7F), Status::Unknown(0x7F));
    }

    #[test]
    fn only_successful_and_busy_are_ok() {
        assert!(Status::Successful.is_ok());
        assert!(Status::Busy.is_ok());
        assert!(!Status::New.is_ok());
        assert!(!Status::Failure.is_ok());
        assert!(!Status::Timeout.is_ok());
        assert!(!Status::NotSupported.is_ok());
        assert!(!Status::Unknown(0x42).is_ok());
    }

    /// Acceptance criterion 28.
    #[test]
    fn device_mode_0x02_is_not_representable() {
        assert_eq!(DeviceMode::from_u8(0x02), None);
        assert_eq!(DeviceMode::from_u8(0x00), Some(DeviceMode::Normal));
        assert_eq!(DeviceMode::from_u8(0x03), Some(DeviceMode::Driver));
        assert_eq!(DeviceMode::from_u8(0x01), None);
        assert_eq!(DeviceMode::Normal.to_u8(), 0x00);
        assert_eq!(DeviceMode::Driver.to_u8(), 0x03);
    }

    #[test]
    fn storage_and_led_discriminants_match_the_c_defines() {
        assert_eq!(Storage::NoStore as u8, 0x00);
        assert_eq!(Storage::VarStore as u8, 0x01);
        assert_eq!(LedId::Zero as u8, 0x00);
        assert_eq!(LedId::ScrollWheel as u8, 0x01);
        assert_eq!(LedId::Logo as u8, 0x04);
        assert_eq!(LedId::Backlight as u8, 0x05);
    }

    const ALL_RATES: [PollRate; 7] = [
        PollRate::Hz125,
        PollRate::Hz250,
        PollRate::Hz500,
        PollRate::Hz1000,
        PollRate::Hz2000,
        PollRate::Hz4000,
        PollRate::Hz8000,
    ];

    #[test]
    fn poll_rate_hz_round_trips() {
        for r in ALL_RATES {
            assert_eq!(PollRate::from_hz(r.hz()), Some(r));
        }
        assert_eq!(PollRate::from_hz(333), None);
        assert_eq!(PollRate::from_hz(0), None);
    }

    #[test]
    fn legacy_codes_match_the_c_switch() {
        assert_eq!(PollRate::Hz1000.legacy_code(), Some(0x01));
        assert_eq!(PollRate::Hz500.legacy_code(), Some(0x02));
        assert_eq!(PollRate::Hz125.legacy_code(), Some(0x08));
        assert_eq!(PollRate::Hz250.legacy_code(), None);
        assert_eq!(PollRate::Hz2000.legacy_code(), None);
        assert_eq!(PollRate::Hz4000.legacy_code(), None);
        assert_eq!(PollRate::Hz8000.legacy_code(), None);

        for r in ALL_RATES {
            if let Some(c) = r.legacy_code() {
                assert_eq!(PollRate::from_legacy_code(c), Some(r));
            }
        }
        assert_eq!(PollRate::from_legacy_code(0x40), None);
    }

    #[test]
    fn v2_codes_match_the_c_switch() {
        assert_eq!(PollRate::Hz8000.v2_code(), 0x01);
        assert_eq!(PollRate::Hz4000.v2_code(), 0x02);
        assert_eq!(PollRate::Hz2000.v2_code(), 0x04);
        assert_eq!(PollRate::Hz1000.v2_code(), 0x08);
        assert_eq!(PollRate::Hz500.v2_code(), 0x10);
        assert_eq!(PollRate::Hz250.v2_code(), 0x20);
        assert_eq!(PollRate::Hz125.v2_code(), 0x40);

        for r in ALL_RATES {
            assert_eq!(PollRate::from_v2_code(r.v2_code()), Some(r));
        }
        assert_eq!(PollRate::from_v2_code(0x00), None);
        assert_eq!(PollRate::from_v2_code(0x03), None);
    }

    #[test]
    fn rgb_new_keeps_channel_order() {
        let c = Rgb::new(0x11, 0x22, 0x33);
        assert_eq!((c.r, c.g, c.b), (0x11, 0x22, 0x33));
    }
}
