// SPDX-License-Identifier: GPL-2.0-or-later
//! `razer-proto` — the Razer HID report wire format, in pure Rust.
//!
//! This crate is a byte-for-byte reimplementation of the report encoding used by
//! the OpenRazer kernel drivers, informed by their GPL source (hence the
//! matching licence). It is **pure**: no I/O, no device access, no filesystem,
//! no dependencies, no `unchecked` blocks. Everything here can be exercised
//! entirely in memory, which is the point — the encoding either is correct or
//! the rest of the project is worthless, so it must be testable without any
//! hardware anywhere near it.
//!
//! # Layout
//!
//! - [`report`] — the 90-byte [`RazerReport`] and its value types.
//! - [`crc`] — the checksum over bytes 2..=87.
//! - [`cmd`] — command constructors, ported from `razerchromacommon.c`.
//! - [`parse`] — response validation and payload decoding.
//! - [`error`] — [`ProtoError`].
//!
//! # The transaction id
//!
//! Byte 1 of every report is the transaction id, and **nothing in this crate
//! ever chooses one**. Constructors in [`cmd`] leave it at `0x00`; the transport
//! stamps it from the per-device table via
//! [`RazerReport::with_transaction_id`].
//!
//! That discipline is the reason this project exists. Upstream's
//! `razer_attr_write_device_mode()` (`razerkbd_driver.c:4290-4315`) hardcodes
//! `0xFF` at line 4308, even though the same driver's `razer_set_device_mode()`
//! switch (`razerkbd_driver.c:536/546`) uses `0x1F` for the BlackWidow V4 Pro,
//! as does every other code path for that device. Because the checksum covers
//! bytes 2..=87 only, byte 1 being wrong yields a *checksum-valid packet with a
//! bogus transaction id* — the device accepts the frame and then does something
//! unwanted with it, which is consistent with the observed firmware reset and
//! USB drop. Nothing upstream catches it, and nothing here can reintroduce it.
//!
//! # Example
//!
//! ```
//! use razer_proto::{cmd, verify_crc, DeviceMode};
//!
//! // The transaction id comes from the caller, never from the constructor.
//! let bytes = cmd::set_device_mode(DeviceMode::Driver)
//!     .with_transaction_id(0x1F) // BlackWidow V4 Pro, razerkbd_driver.c:546
//!     .to_bytes();
//!
//! assert_eq!(bytes.len(), 90);
//! assert_eq!(bytes[1], 0x1F);
//! assert_eq!(bytes[88], 0x05); // 0x02 ^ 0x00 ^ 0x04 ^ 0x03
//! assert!(verify_crc(&bytes));
//! ```

#![warn(missing_docs)]
#![warn(clippy::all)]

pub mod cmd;
pub mod crc;
pub mod error;
pub mod parse;
pub mod report;

pub use crate::crc::{crc, verify_crc};
pub use crate::error::ProtoError;
pub use crate::report::{
    ARGS_LEN, DeviceMode, LedId, PollRate, REPORT_LEN, RazerReport, Rgb, Status, Storage,
};

#[cfg(test)]
mod tests {
    use super::*;

    /// The re-export surface `razer-hid` compiles against. If this stops
    /// building, the frozen API has been broken.
    #[test]
    fn public_surface_is_reachable_from_the_crate_root() {
        assert_eq!(REPORT_LEN, 90);
        assert_eq!(ARGS_LEN, 80);

        let report: RazerReport = cmd::set_static_effect(
            Storage::VarStore,
            LedId::Backlight,
            Rgb::new(0xFF, 0x00, 0x00),
        );
        let bytes = report.to_bytes();
        assert!(verify_crc(&bytes));
        assert_eq!(crc(&bytes), bytes[88]);

        let status: Status = Status::from_u8(bytes[0]);
        assert_eq!(status, Status::New);

        let mode: DeviceMode = DeviceMode::Driver;
        assert_eq!(mode.to_u8(), 0x03);

        let rate: PollRate = PollRate::Hz1000;
        assert_eq!(rate.hz(), 1000);

        let err: ProtoError = ProtoError::DataSizeTooLarge { got: 81 };
        assert!(!err.to_string().is_empty());
    }

    /// Acceptance criterion 27, asserted on the struct field itself.
    ///
    /// `cmd.rs` deliberately never names this field — that file is grepped to
    /// prove it — so the field-level check lives here.
    #[test]
    fn no_command_constructor_sets_a_transaction_id() {
        let reports = [
            cmd::set_device_mode(DeviceMode::Normal),
            cmd::set_device_mode(DeviceMode::Driver),
            cmd::get_device_mode(),
            cmd::get_firmware_version(),
            cmd::get_serial(),
            cmd::set_brightness(Storage::VarStore, LedId::Backlight, 0x80),
            cmd::get_brightness(Storage::VarStore, LedId::Backlight),
            cmd::set_static_effect(Storage::VarStore, LedId::Zero, Rgb::new(1, 2, 3)),
            cmd::set_effect_none(Storage::VarStore, LedId::Backlight),
            cmd::set_poll_rate_legacy(PollRate::Hz1000).expect("1000 Hz is legacy-encodable"),
            cmd::get_poll_rate_legacy(),
            cmd::set_poll_rate_v2(PollRate::Hz1000, 0x00),
            cmd::get_poll_rate_v2(),
        ];
        for r in reports {
            assert_eq!(
                r.transaction_id, 0x00,
                "class {:#04x} id {:#04x} came out of cmd with an id already stamped",
                r.command_class, r.command_id
            );
        }
    }

    /// The full BlackWidow V4 Pro "enter driver mode" frame, which is the exact
    /// command upstream gets wrong. Duplicated deliberately from `report.rs`:
    /// this is the one byte string the whole project hangs on.
    #[test]
    fn blackwidow_v4_pro_driver_mode_frame() {
        let bytes = cmd::set_device_mode(DeviceMode::Driver)
            .with_transaction_id(0x1F)
            .to_bytes();

        assert_eq!(
            &bytes[0..10],
            &[0x00, 0x1F, 0x00, 0x00, 0x00, 0x02, 0x00, 0x04, 0x03, 0x00]
        );
        assert!(bytes[10..88].iter().all(|&b| b == 0));
        assert_eq!(bytes[88], 0x05);
        assert_eq!(bytes[89], 0x00);
    }
}
