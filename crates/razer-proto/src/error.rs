// SPDX-License-Identifier: GPL-2.0-or-later
//! Error type for the Razer wire protocol.
//!
//! Mirrors the failure modes the C driver detects in `razer_get_usb_response()`
//! (`razercommon.c:63-100`) and `razer_send_payload()` (`razerkbd_driver.c:420-480`).

use crate::report::Status;

/// Something went wrong encoding, decoding or validating a Razer report.
///
/// This crate performs no I/O, so every variant here describes a problem with
/// *bytes*, never with a transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtoError {
    /// `data_size` exceeds the 80-byte `arguments` array.
    ///
    /// The C driver sanitises this in `razer_get_usb_response()`
    /// (`razercommon.c:93-99`) and returns `-EINVAL`.
    DataSizeTooLarge {
        /// The offending `data_size` value.
        got: u8,
    },
    /// The response did not echo back the request's routing fields.
    ///
    /// `razer_send_payload()` requires `remaining_packets`, `command_class` and
    /// `command_id` to match the request before it will look at the status.
    ResponseMismatch {
        /// Which field disagreed: `"remaining_packets"`, `"command_class"` or
        /// `"command_id"`.
        field: &'static str,
        /// The value taken from the request.
        expected: u32,
        /// The value found in the response.
        got: u32,
    },
    /// The device answered with a status that is neither `Successful` nor `Busy`.
    DeviceStatus(Status),
    /// The checksum byte at index 88 does not match the computed checksum.
    ///
    /// The C driver never checks response checksums; this is ours to use, and
    /// callers must opt in to treating it as fatal.
    BadCrc {
        /// The checksum computed over bytes 2..=87.
        expected: u8,
        /// The checksum byte actually present at index 88.
        got: u8,
    },
    /// A parse helper was handed bytes it could not interpret.
    Malformed(&'static str),
}

impl core::fmt::Display for ProtoError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::DataSizeTooLarge { got } => {
                write!(f, "data_size {got} exceeds the 80-byte argument array")
            }
            Self::ResponseMismatch {
                field,
                expected,
                got,
            } => write!(
                f,
                "response {field} mismatch: expected {expected:#04x}, got {got:#04x}"
            ),
            Self::DeviceStatus(status) => {
                write!(f, "device returned status {:#04x}", status.to_u8())
            }
            Self::BadCrc { expected, got } => {
                write!(f, "bad crc: expected {expected:#04x}, got {got:#04x}")
            }
            Self::Malformed(what) => write!(f, "malformed report: {what}"),
        }
    }
}

impl std::error::Error for ProtoError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_is_non_empty_for_every_variant() {
        let variants = [
            ProtoError::DataSizeTooLarge { got: 81 },
            ProtoError::ResponseMismatch {
                field: "command_class",
                expected: 0x00,
                got: 0x0F,
            },
            ProtoError::DeviceStatus(Status::NotSupported),
            ProtoError::BadCrc {
                expected: 0x05,
                got: 0x00,
            },
            ProtoError::Malformed("nope"),
        ];
        for v in variants {
            assert!(!v.to_string().is_empty());
        }
    }

    #[test]
    fn is_a_std_error() {
        fn assert_error<E: std::error::Error>(_: &E) {}
        assert_error(&ProtoError::Malformed("x"));
    }
}
