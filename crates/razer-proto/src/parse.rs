// SPDX-License-Identifier: GPL-2.0-or-later
//! Response validation and payload decoding.
//!
//! [`check_response`] reproduces the acceptance test in `razer_send_payload()`
//! (`razerkbd_driver.c:420-480`, and the identical logic in
//! `razermouse_driver.c`). The per-field readers below each mirror one sysfs
//! reader in the C driver, and the argument indices they use are the fiddly bit:
//! brightness lives at `arguments[2]`, the legacy poll rate at `arguments[0]`
//! and the v2 poll rate at `arguments[1]`.

use crate::error::ProtoError;
use crate::report::{ARGS_LEN, PollRate, RazerReport};

/// Reject a report whose `data_size` cannot fit in the argument array.
///
/// `razer_get_usb_response()` (`razercommon.c:93-99`) sanitises this and returns
/// `-EINVAL`.
fn check_data_size(report: &RazerReport) -> Result<(), ProtoError> {
    if report.data_size as usize > ARGS_LEN {
        return Err(ProtoError::DataSizeTooLarge {
            got: report.data_size,
        });
    }
    Ok(())
}

/// Validate a response against the request that produced it.
///
/// Mirrors `razer_send_payload()` (`razerkbd_driver.c:436-449`):
///
/// 1. `remaining_packets`, `command_class` and `command_id` must all match the
///    request, otherwise *"Response doesn't match request"*.
/// 2. The status must then be `RAZER_CMD_SUCCESSFUL` (0x02) **or**
///    `RAZER_CMD_BUSY` (0x01) — upstream treats busy as success.
///
/// The checksum is deliberately *not* checked: the C driver never verifies a
/// response checksum, so treating a mismatch as fatal here would reject frames
/// real hardware sends. Use [`crate::verify_crc`] separately if you want that,
/// behind an explicit opt-in.
///
/// # Errors
///
/// - [`ProtoError::DataSizeTooLarge`] if either report declares a `data_size`
///   above 80.
/// - [`ProtoError::ResponseMismatch`] if a routing field disagrees.
/// - [`ProtoError::DeviceStatus`] if the device reported anything other than
///   successful or busy.
pub fn check_response(request: &RazerReport, response: &RazerReport) -> Result<(), ProtoError> {
    check_data_size(request)?;
    check_data_size(response)?;

    if response.remaining_packets != request.remaining_packets {
        return Err(ProtoError::ResponseMismatch {
            field: "remaining_packets",
            expected: u32::from(request.remaining_packets),
            got: u32::from(response.remaining_packets),
        });
    }
    if response.command_class != request.command_class {
        return Err(ProtoError::ResponseMismatch {
            field: "command_class",
            expected: u32::from(request.command_class),
            got: u32::from(response.command_class),
        });
    }
    if response.command_id != request.command_id {
        return Err(ProtoError::ResponseMismatch {
            field: "command_id",
            expected: u32::from(request.command_id),
            got: u32::from(response.command_id),
        });
    }

    if response.status.is_ok() {
        Ok(())
    } else {
        Err(ProtoError::DeviceStatus(response.status))
    }
}

/// The 22-byte ASCII serial from `arguments[0..22]`.
///
/// `razer_attr_read_device_serial` (`razerkbd_driver.c:2143-2169`) does
/// `memcpy(serial_string, response.arguments, 22)` then NUL-terminates at index
/// 22. We trim at the first NUL and strip trailing whitespace, which the C code
/// gets for free from `sysfs_emit`'s `%s`.
///
/// # Errors
///
/// [`ProtoError::Malformed`] if the bytes before the first NUL are not ASCII.
pub fn serial(response: &RazerReport) -> Result<String, ProtoError> {
    let raw = &response.arguments[..22];
    let end = raw.iter().position(|&b| b == 0x00).unwrap_or(raw.len());
    let trimmed = &raw[..end];

    if !trimmed.is_ascii() {
        return Err(ProtoError::Malformed("serial is not ASCII"));
    }

    // Every byte is ASCII, so this is a lossless conversion.
    let text: String = trimmed.iter().map(|&b| b as char).collect();
    Ok(text.trim_end().to_string())
}

/// `(major, minor)` from `arguments[0]` and `arguments[1]`.
///
/// `razer_attr_read_firmware_version` (`razerkbd_driver.c`) prints these as
/// `"v%d.%d"`.
///
/// # Errors
///
/// Never fails today; the `Result` is kept so a future stricter check does not
/// break callers.
pub fn firmware_version(response: &RazerReport) -> Result<(u8, u8), ProtoError> {
    Ok((response.arguments[0], response.arguments[1]))
}

/// `(mode_byte, param_byte)` from `arguments[0]` and `arguments[1]`.
///
/// Returned raw rather than as a [`crate::DeviceMode`] because a device may
/// legitimately report the blocked `0x02` mode, and swallowing that would hide
/// exactly the sort of state this project is trying to diagnose.
///
/// # Errors
///
/// Never fails today; the `Result` is kept for forward compatibility.
pub fn device_mode(response: &RazerReport) -> Result<(u8, u8), ProtoError> {
    Ok((response.arguments[0], response.arguments[1]))
}

/// Brightness from `arguments[2]` — **not** `arguments[0]`.
///
/// `razer_attr_read_matrix_brightness` (`razerkbd_driver.c:4276-4282`):
/// `brightness = response.arguments[2]` on the non-Blade branch. The Blade
/// laptops read `arguments[1]` instead; no Blade is in scope, so that branch is
/// not reproduced.
///
/// # Errors
///
/// Never fails today; the `Result` is kept for forward compatibility.
pub fn brightness(response: &RazerReport) -> Result<u8, ProtoError> {
    Ok(response.arguments[2])
}

/// Polling rate from `arguments[0]`, legacy encoding.
///
/// `razer_chroma_misc_get_polling_rate` (`razerchromacommon.c:1085-1095`):
/// *"Identifier is in arg[0]"*.
///
/// # Errors
///
/// [`ProtoError::Malformed`] if the byte is not a legacy code.
pub fn poll_rate_legacy(response: &RazerReport) -> Result<PollRate, ProtoError> {
    PollRate::from_legacy_code(response.arguments[0])
        .ok_or(ProtoError::Malformed("unknown legacy poll rate code"))
}

/// Polling rate from `arguments[1]`, v2 encoding.
///
/// `razer_chroma_misc_get_polling_rate2` (`razerchromacommon.c:1124-1141`):
/// *"Identifier is in arg[1]"*. Note the index differs from legacy — reading
/// index 0 here would return the device's echo of the `argument` byte.
///
/// # Errors
///
/// [`ProtoError::Malformed`] if the byte is not a v2 code.
pub fn poll_rate_v2(response: &RazerReport) -> Result<PollRate, ProtoError> {
    PollRate::from_v2_code(response.arguments[1])
        .ok_or(ProtoError::Malformed("unknown v2 poll rate code"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd;
    use crate::report::{DeviceMode, LedId, Status, Storage};

    /// Build a plausible response to `request`: same routing fields, given
    /// status, given payload.
    fn respond(request: &RazerReport, status: Status, args: &[u8]) -> RazerReport {
        let mut r = *request;
        r.status = status;
        r.arguments = [0u8; ARGS_LEN];
        r.arguments[..args.len()].copy_from_slice(args);
        r
    }

    /// Acceptance criterion 29.
    #[test]
    fn successful_and_busy_are_both_accepted() {
        let req = cmd::get_device_mode();
        for status in [Status::Successful, Status::Busy] {
            let resp = respond(&req, status, &[0x03, 0x00]);
            assert_eq!(check_response(&req, &resp), Ok(()));
        }
    }

    /// Acceptance criterion 30.
    #[test]
    fn mismatches_and_bad_statuses_are_rejected() {
        let req = cmd::get_device_mode();

        let mut wrong_class = respond(&req, Status::Successful, &[]);
        wrong_class.command_class = 0x0F;
        assert_eq!(
            check_response(&req, &wrong_class),
            Err(ProtoError::ResponseMismatch {
                field: "command_class",
                expected: 0x00,
                got: 0x0F,
            })
        );

        let mut wrong_id = respond(&req, Status::Successful, &[]);
        wrong_id.command_id = 0x04;
        assert_eq!(
            check_response(&req, &wrong_id),
            Err(ProtoError::ResponseMismatch {
                field: "command_id",
                expected: 0x84,
                got: 0x04,
            })
        );

        let mut wrong_packets = respond(&req, Status::Successful, &[]);
        wrong_packets.remaining_packets = 0x0001;
        assert_eq!(
            check_response(&req, &wrong_packets),
            Err(ProtoError::ResponseMismatch {
                field: "remaining_packets",
                expected: 0,
                got: 1,
            })
        );

        let not_supported = respond(&req, Status::NotSupported, &[]);
        assert_eq!(
            check_response(&req, &not_supported),
            Err(ProtoError::DeviceStatus(Status::NotSupported))
        );

        for status in [
            Status::New,
            Status::Failure,
            Status::Timeout,
            Status::Unknown(0x42),
        ] {
            let resp = respond(&req, status, &[]);
            assert_eq!(
                check_response(&req, &resp),
                Err(ProtoError::DeviceStatus(status))
            );
        }
    }

    /// Acceptance criterion 35.
    #[test]
    fn oversized_data_size_is_rejected() {
        let req = cmd::get_device_mode();

        let mut fat_response = respond(&req, Status::Successful, &[]);
        fat_response.data_size = 81;
        assert_eq!(
            check_response(&req, &fat_response),
            Err(ProtoError::DataSizeTooLarge { got: 81 })
        );

        let mut fat_request = req;
        fat_request.data_size = 81;
        let resp = respond(&fat_request, Status::Successful, &[]);
        assert_eq!(
            check_response(&fat_request, &resp),
            Err(ProtoError::DataSizeTooLarge { got: 81 })
        );

        // 80 is the limit and must be accepted.
        let mut at_limit = req;
        at_limit.data_size = 80;
        let ok = respond(&at_limit, Status::Successful, &[]);
        assert_eq!(check_response(&at_limit, &ok), Ok(()));
    }

    /// Acceptance criterion 31 — brightness is at index 2, not index 0.
    #[test]
    fn brightness_reads_argument_two() {
        let req = cmd::get_brightness(Storage::VarStore, LedId::Backlight);
        let resp = respond(&req, Status::Successful, &[0x01, 0x05, 0x42]);
        assert_eq!(brightness(&resp), Ok(0x42));
        assert_ne!(brightness(&resp), Ok(0x01));
    }

    /// Acceptance criterion 32 — the two poll-rate encodings read different
    /// indices, proven on one shared buffer.
    #[test]
    fn poll_rate_readers_use_different_indices() {
        let req = cmd::get_poll_rate_v2();
        // arguments[0] = 0x01 (a legacy code for 1000 Hz)
        // arguments[1] = 0x10 (a v2 code for 500 Hz)
        let resp = respond(&req, Status::Successful, &[0x01, 0x10]);

        assert_eq!(poll_rate_legacy(&resp), Ok(PollRate::Hz1000));
        assert_eq!(poll_rate_v2(&resp), Ok(PollRate::Hz500));
        assert_ne!(poll_rate_legacy(&resp), poll_rate_v2(&resp));
    }

    #[test]
    fn poll_rate_readers_reject_unknown_codes() {
        let req = cmd::get_poll_rate_legacy();
        let resp = respond(&req, Status::Successful, &[0x7E, 0x7E]);
        assert!(matches!(
            poll_rate_legacy(&resp),
            Err(ProtoError::Malformed(_))
        ));
        assert!(matches!(poll_rate_v2(&resp), Err(ProtoError::Malformed(_))));
    }

    /// Acceptance criterion 33.
    #[test]
    fn serial_trims_at_the_nul() {
        let req = cmd::get_serial();
        let resp = respond(&req, Status::Successful, b"XX1234567890123456789\0");
        let s = serial(&resp).expect("ASCII serial");
        assert_eq!(s, "XX1234567890123456789");
        assert_eq!(s.len(), 21);
        assert!(!s.contains('\0'));
        assert_eq!(s.trim_end(), s);
    }

    #[test]
    fn serial_trims_trailing_whitespace_and_handles_a_full_field() {
        let req = cmd::get_serial();

        let padded = respond(&req, Status::Successful, b"PM12345678   \0");
        assert_eq!(serial(&padded).expect("ASCII"), "PM12345678");

        // Exactly 22 bytes with no NUL at all.
        let full = respond(&req, Status::Successful, b"0123456789ABCDEFGHIJKL");
        assert_eq!(serial(&full).expect("ASCII"), "0123456789ABCDEFGHIJKL");

        // Byte 22 onwards must be ignored.
        let mut bleed = respond(&req, Status::Successful, b"0123456789ABCDEFGHIJKL");
        bleed.arguments[22] = b'Z';
        assert_eq!(serial(&bleed).expect("ASCII"), "0123456789ABCDEFGHIJKL");

        let empty = respond(&req, Status::Successful, &[]);
        assert_eq!(serial(&empty).expect("ASCII"), "");
    }

    #[test]
    fn serial_rejects_non_ascii() {
        let req = cmd::get_serial();
        let resp = respond(&req, Status::Successful, &[0xFF, 0xFE, 0x00]);
        assert_eq!(
            serial(&resp),
            Err(ProtoError::Malformed("serial is not ASCII"))
        );
    }

    /// Acceptance criterion 34.
    #[test]
    fn firmware_version_reads_the_first_two_arguments() {
        let req = cmd::get_firmware_version();
        let resp = respond(&req, Status::Successful, &[0x01, 0x0B, 0xFF]);
        assert_eq!(firmware_version(&resp), Ok((1, 11)));
    }

    #[test]
    fn device_mode_returns_the_raw_pair() {
        let req = cmd::get_device_mode();

        let driver = respond(&req, Status::Successful, &[0x03, 0x00]);
        assert_eq!(device_mode(&driver), Ok((0x03, 0x00)));
        assert_eq!(
            DeviceMode::from_u8(device_mode(&driver).expect("pair").0),
            Some(DeviceMode::Driver)
        );

        // The blocked 0x02 mode is reported raw rather than swallowed.
        let odd = respond(&req, Status::Successful, &[0x02, 0x00]);
        assert_eq!(device_mode(&odd), Ok((0x02, 0x00)));
        assert_eq!(DeviceMode::from_u8(0x02), None);
    }

    /// A full request/response cycle over encoded bytes, the way the transport
    /// will actually use this crate.
    #[test]
    fn end_to_end_over_the_wire_form() {
        let request = cmd::set_device_mode(DeviceMode::Driver).with_transaction_id(0x1F);
        let request_bytes = request.to_bytes();
        assert!(crate::verify_crc(&request_bytes));

        // Device echoes the routing fields back with a success status.
        let mut reply_bytes = request_bytes;
        reply_bytes[0] = Status::Successful.to_u8();
        reply_bytes[88] = crate::crc::crc(&reply_bytes);

        let response = RazerReport::from_bytes(&reply_bytes);
        assert_eq!(check_response(&request, &response), Ok(()));
        assert_eq!(device_mode(&response), Ok((0x03, 0x00)));
    }
}
