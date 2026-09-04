// SPDX-License-Identifier: GPL-2.0-or-later
//! Command constructors — ports of the `razer_chroma_*` builders in
//! `razerchromacommon.c`.
//!
//! # The one rule in this module
//!
//! Every constructor here returns a report whose byte 1 is `0x00`. **Nothing in
//! this module ever stamps that byte.** The value comes from the per-device
//! table, at send time, in the transport layer.
//!
//! That is the entire point of this project. Upstream's
//! `razer_attr_write_device_mode()` (`razerkbd_driver.c:4290-4315`) hardcodes
//! `0xFF` on line 4308, while its own `razer_set_device_mode()` per-device
//! switch (`razerkbd_driver.c:536/546`) uses `0x1F` for the BlackWidow V4 Pro,
//! as does every other V4 Pro code path. The resulting frame is checksum-valid —
//! byte 1 is outside the checksum range — so the device accepts it and then does
//! something unwanted with it. Keeping that byte out of this module by
//! construction means the bug cannot be reintroduced here.

use crate::error::ProtoError;
use crate::report::{DeviceMode, LedId, RazerReport, Rgb, Storage};

/*
 * Standard device commands (command class 0x00)
 */

/// Set the operating mode.
///
/// `razer_chroma_standard_set_device_mode`, `razerchromacommon.c:25-44`:
/// class 0x00, id 0x04, `data_size` 0x02; `arguments[0]` = mode,
/// `arguments[1]` = 0x00 (upstream forces the param to zero).
pub fn set_device_mode(mode: DeviceMode) -> RazerReport {
    RazerReport::new(0x00, 0x04, 0x02).with_args(&[mode.to_u8(), 0x00])
}

/// Read back the operating mode.
///
/// `razer_chroma_standard_get_device_mode`, `razerchromacommon.c:51-54`.
pub fn get_device_mode() -> RazerReport {
    RazerReport::new(0x00, 0x84, 0x02)
}

/// Read the firmware version.
///
/// `razer_chroma_standard_get_firmware_version`, `razerchromacommon.c:67-70`.
/// The response carries the major in `arguments[0]` and the minor in
/// `arguments[1]`.
pub fn get_firmware_version() -> RazerReport {
    RazerReport::new(0x00, 0x81, 0x02)
}

/// Read the device serial number.
///
/// `razer_chroma_standard_get_serial`, `razerchromacommon.c:59-62`. The response
/// carries 22 ASCII bytes at `arguments[0..22]`.
pub fn get_serial() -> RazerReport {
    RazerReport::new(0x00, 0x82, 0x16)
}

/*
 * Extended matrix commands (command class 0x0F)
 */

/// Set LED brightness.
///
/// `razer_chroma_extended_matrix_brightness`, `razerchromacommon.c:714-722`:
/// class 0x0F, id 0x04, `data_size` 0x03; args `[storage, led, brightness]`.
pub fn set_brightness(storage: Storage, led: LedId, brightness: u8) -> RazerReport {
    RazerReport::new(0x0F, 0x04, 0x03).with_args(&[storage as u8, led as u8, brightness])
}

/// Read back LED brightness.
///
/// `razer_chroma_extended_matrix_get_brightness`, `razerchromacommon.c:731-738`:
/// class 0x0F, id 0x84, `data_size` 0x03; args `[storage, led]`.
///
/// Note the response puts the brightness in `arguments[2]`, not `arguments[0]` —
/// see [`crate::parse::brightness`].
pub fn get_brightness(storage: Storage, led: LedId) -> RazerReport {
    RazerReport::new(0x0F, 0x84, 0x03).with_args(&[storage as u8, led as u8])
}

/// Set the "static colour" effect.
///
/// `razer_chroma_extended_matrix_effect_static`, `razerchromacommon.c:511-520`,
/// built on `razer_chroma_extended_matrix_effect_base` at `:481-490`:
/// class 0x0F, id 0x02, `data_size` 0x09; args
/// `[storage, led, 0x01, 0x00, 0x00, 0x01, r, g, b]`.
///
/// The comment at `razerchromacommon.c:505-509` spells the payload out as
/// `010501000001ff0000` for VARSTORE / BACKLIGHT / red.
pub fn set_static_effect(storage: Storage, led: LedId, rgb: Rgb) -> RazerReport {
    RazerReport::new(0x0F, 0x02, 0x09).with_args(&[
        storage as u8,
        led as u8,
        0x01, // effect id: static
        0x00,
        0x00,
        0x01, // upstream sets arguments[5] to 0x01; purpose undocumented
        rgb.r,
        rgb.g,
        rgb.b,
    ])
}

/// Set the "none" effect — LEDs off.
///
/// `razer_chroma_extended_matrix_effect_none`, `razerchromacommon.c:498-501`:
/// class 0x0F, id 0x02, `data_size` 0x06; args `[storage, led, 0x00]` with the
/// remaining three payload bytes left zero.
pub fn set_effect_none(storage: Storage, led: LedId) -> RazerReport {
    RazerReport::new(0x0F, 0x02, 0x06).with_args(&[storage as u8, led as u8, 0x00])
}

/*
 * Polling rate — two mutually incompatible encodings
 */

/// Set the polling rate, legacy encoding.
///
/// `razer_chroma_misc_set_polling_rate`, `razerchromacommon.c:1104-1136`:
/// class 0x00, id 0x05, `data_size` 0x01; `arguments[0]` = the legacy code.
///
/// Used by the Basilisk V3 Pro.
///
/// # Errors
///
/// Returns [`ProtoError::Malformed`] if the rate has no legacy encoding.
/// Upstream silently substitutes 500 Hz here; we refuse rather than set a rate
/// the caller did not ask for.
pub fn set_poll_rate_legacy(rate: crate::report::PollRate) -> Result<RazerReport, ProtoError> {
    let code = rate
        .legacy_code()
        .ok_or(ProtoError::Malformed("poll rate has no legacy encoding"))?;
    Ok(RazerReport::new(0x00, 0x05, 0x01).with_args(&[code]))
}

/// Read back the polling rate, legacy encoding.
///
/// `razer_chroma_misc_get_polling_rate`, `razerchromacommon.c:1092-1095`. The
/// response carries the code in `arguments[0]`.
pub fn get_poll_rate_legacy() -> RazerReport {
    RazerReport::new(0x00, 0x85, 0x01)
}

/// Set the polling rate, v2 encoding.
///
/// `razer_chroma_misc_set_polling_rate2`, `razerchromacommon.c:1153-1189`:
/// class 0x00, id 0x40, `data_size` 0x02; args `[argument, code]`.
///
/// Used by the BlackWidow V4 Pro. Callers in scope pass `argument = 0x00` — the
/// keyboard's `razer_attr_write_poll_rate` does exactly that. Upstream notes
/// that some devices want the request sent twice, once with 0x00 and once with
/// 0x01, hence the parameter.
pub fn set_poll_rate_v2(rate: crate::report::PollRate, argument: u8) -> RazerReport {
    RazerReport::new(0x00, 0x40, 0x02).with_args(&[argument, rate.v2_code()])
}

/// Read back the polling rate, v2 encoding.
///
/// `razer_chroma_misc_get_polling_rate2`, `razerchromacommon.c:1138-1141`. The
/// response carries the code in `arguments[1]` — note the index differs from
/// legacy.
pub fn get_poll_rate_v2() -> RazerReport {
    RazerReport::new(0x00, 0xC0, 0x01)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::PollRate;

    /// Assert class, id, data_size and the meaningful payload in one go.
    fn assert_cmd(r: &RazerReport, class: u8, id: u8, data_size: u8, args: &[u8]) {
        assert_eq!(r.command_class, class, "command class");
        assert_eq!(r.command_id, id, "command id");
        assert_eq!(r.data_size, data_size, "data size");
        assert_eq!(r.args(), args, "arguments");
    }

    /// Acceptance criterion 15.
    #[test]
    fn set_device_mode_payloads() {
        assert_cmd(
            &set_device_mode(DeviceMode::Normal),
            0x00,
            0x04,
            0x02,
            &[0x00, 0x00],
        );
        assert_cmd(
            &set_device_mode(DeviceMode::Driver),
            0x00,
            0x04,
            0x02,
            &[0x03, 0x00],
        );
    }

    /// Acceptance criteria 16-18.
    #[test]
    fn standard_getters() {
        assert_cmd(&get_device_mode(), 0x00, 0x84, 0x02, &[0x00, 0x00]);
        assert_cmd(&get_firmware_version(), 0x00, 0x81, 0x02, &[0x00, 0x00]);
        assert_cmd(&get_serial(), 0x00, 0x82, 0x16, &[0x00; 0x16]);
    }

    /// Acceptance criterion 19.
    #[test]
    fn set_brightness_payload() {
        assert_cmd(
            &set_brightness(Storage::VarStore, LedId::Backlight, 0x80),
            0x0F,
            0x04,
            0x03,
            &[0x01, 0x05, 0x80],
        );
        // The Basilisk V3 Pro's variant.
        assert_cmd(
            &set_brightness(Storage::VarStore, LedId::Zero, 0xFF),
            0x0F,
            0x04,
            0x03,
            &[0x01, 0x00, 0xFF],
        );
    }

    /// Acceptance criterion 20.
    #[test]
    fn get_brightness_payload() {
        let r = get_brightness(Storage::VarStore, LedId::Backlight);
        // Upstream sets only the first two argument bytes but declares data_size 3.
        assert_eq!(&r.args()[..2], &[0x01, 0x05]);
        assert_cmd(&r, 0x0F, 0x84, 0x03, &[0x01, 0x05, 0x00]);
    }

    /// Acceptance criterion 21 — the `010501000001ff0000` string from the
    /// `razerchromacommon.c:505-509` comment.
    #[test]
    fn static_effect_matches_the_documented_byte_string() {
        let r = set_static_effect(
            Storage::VarStore,
            LedId::Backlight,
            Rgb::new(0xFF, 0x00, 0x00),
        );
        assert_cmd(
            &r,
            0x0F,
            0x02,
            0x09,
            &[0x01, 0x05, 0x01, 0x00, 0x00, 0x01, 0xFF, 0x00, 0x00],
        );

        // And the green variant from the same comment: 01050100000100ff00
        let g = set_static_effect(
            Storage::VarStore,
            LedId::Backlight,
            Rgb::new(0x00, 0xFF, 0x00),
        );
        assert_eq!(
            g.args(),
            &[0x01, 0x05, 0x01, 0x00, 0x00, 0x01, 0x00, 0xFF, 0x00]
        );
    }

    /// Acceptance criterion 22 — `010500000000` from `razerchromacommon.c:495`.
    #[test]
    fn effect_none_payload() {
        assert_cmd(
            &set_effect_none(Storage::VarStore, LedId::Backlight),
            0x0F,
            0x02,
            0x06,
            &[0x01, 0x05, 0x00, 0x00, 0x00, 0x00],
        );
    }

    /// Acceptance criterion 23.
    #[test]
    fn legacy_poll_rate_payloads() {
        let r = set_poll_rate_legacy(PollRate::Hz1000).expect("1000 Hz is legacy-encodable");
        assert_cmd(&r, 0x00, 0x05, 0x01, &[0x01]);
        assert_eq!(
            set_poll_rate_legacy(PollRate::Hz500)
                .expect("500 Hz is legacy-encodable")
                .args(),
            &[0x02]
        );
        assert_eq!(
            set_poll_rate_legacy(PollRate::Hz125)
                .expect("125 Hz is legacy-encodable")
                .args(),
            &[0x08]
        );

        for rate in [
            PollRate::Hz250,
            PollRate::Hz2000,
            PollRate::Hz4000,
            PollRate::Hz8000,
        ] {
            assert!(
                matches!(set_poll_rate_legacy(rate), Err(ProtoError::Malformed(_))),
                "{rate:?} must not be legacy-encodable"
            );
        }
    }

    /// Acceptance criterion 24.
    #[test]
    fn get_poll_rate_legacy_shape() {
        assert_cmd(&get_poll_rate_legacy(), 0x00, 0x85, 0x01, &[0x00]);
    }

    /// Acceptance criterion 25.
    #[test]
    fn v2_poll_rate_payloads() {
        let cases = [
            (PollRate::Hz8000, 0x01u8),
            (PollRate::Hz4000, 0x02),
            (PollRate::Hz2000, 0x04),
            (PollRate::Hz1000, 0x08),
            (PollRate::Hz500, 0x10),
            (PollRate::Hz250, 0x20),
            (PollRate::Hz125, 0x40),
        ];
        for (rate, code) in cases {
            assert_cmd(
                &set_poll_rate_v2(rate, 0x00),
                0x00,
                0x40,
                0x02,
                &[0x00, code],
            );
        }
        // The `argument` byte is genuinely the caller's.
        assert_eq!(
            set_poll_rate_v2(PollRate::Hz1000, 0x01).args(),
            &[0x01, 0x08]
        );
    }

    /// Acceptance criterion 26.
    #[test]
    fn get_poll_rate_v2_shape() {
        assert_cmd(&get_poll_rate_v2(), 0x00, 0xC0, 0x01, &[0x00]);
    }

    /// Acceptance criterion 27 — THE regression guard for this module.
    ///
    /// Every constructor must leave byte 1 at zero. If this ever fails, someone
    /// has reintroduced the upstream hardcode.
    #[test]
    fn no_constructor_stamps_an_id() {
        let reports = [
            set_device_mode(DeviceMode::Normal),
            set_device_mode(DeviceMode::Driver),
            get_device_mode(),
            get_firmware_version(),
            get_serial(),
            set_brightness(Storage::VarStore, LedId::Backlight, 0x80),
            get_brightness(Storage::VarStore, LedId::Backlight),
            set_static_effect(Storage::VarStore, LedId::Backlight, Rgb::new(1, 2, 3)),
            set_effect_none(Storage::VarStore, LedId::Backlight),
            set_poll_rate_legacy(PollRate::Hz1000).expect("legacy-encodable"),
            get_poll_rate_legacy(),
            set_poll_rate_v2(PollRate::Hz1000, 0x00),
            get_poll_rate_v2(),
        ];
        for r in reports {
            assert_eq!(
                r.to_bytes()[1],
                0x00,
                "constructor for class {:#04x} id {:#04x} stamped byte 1",
                r.command_class,
                r.command_id
            );
        }
    }

    #[test]
    fn every_constructor_is_well_formed() {
        let reports = [
            set_device_mode(DeviceMode::Driver),
            get_device_mode(),
            get_firmware_version(),
            get_serial(),
            set_brightness(Storage::VarStore, LedId::Backlight, 0x80),
            get_brightness(Storage::VarStore, LedId::Backlight),
            set_static_effect(Storage::VarStore, LedId::Backlight, Rgb::new(1, 2, 3)),
            set_effect_none(Storage::VarStore, LedId::Backlight),
            set_poll_rate_legacy(PollRate::Hz125).expect("legacy-encodable"),
            get_poll_rate_legacy(),
            set_poll_rate_v2(PollRate::Hz8000, 0x00),
            get_poll_rate_v2(),
        ];
        for r in reports {
            assert_eq!(r.status, crate::report::Status::New);
            assert_eq!(r.remaining_packets, 0);
            assert_eq!(r.protocol_type, 0x00);
            assert_eq!(r.reserved, 0x00);
            assert!(r.data_size as usize <= crate::report::ARGS_LEN);
            let bytes = r.to_bytes();
            assert!(crate::crc::verify_crc(&bytes));
        }
    }

    #[test]
    fn getters_have_the_direction_bit_set_and_setters_do_not() {
        for r in [
            get_device_mode(),
            get_firmware_version(),
            get_serial(),
            get_brightness(Storage::VarStore, LedId::Backlight),
            get_poll_rate_legacy(),
            get_poll_rate_v2(),
        ] {
            assert!(r.is_get(), "class {:#04x} should be a get", r.command_class);
        }
        for r in [
            set_device_mode(DeviceMode::Driver),
            set_brightness(Storage::VarStore, LedId::Backlight, 0x80),
            set_static_effect(Storage::VarStore, LedId::Backlight, Rgb::new(1, 2, 3)),
            set_effect_none(Storage::VarStore, LedId::Backlight),
            set_poll_rate_v2(PollRate::Hz1000, 0x00),
        ] {
            assert!(!r.is_get());
        }
    }
}
