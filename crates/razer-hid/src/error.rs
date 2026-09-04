// SPDX-License-Identifier: GPL-2.0-or-later
//! The error type for enumeration and transport.

use razer_proto::ProtoError;

/// Anything that can go wrong finding, opening or talking to a Razer device.
///
/// Protocol-level problems (a response that does not echo the request, a bad
/// status byte) arrive here wrapped in [`HidError::Proto`]; `razer-proto` does
/// no I/O and therefore has no notion of an errno.
#[derive(Debug)]
pub enum HidError {
    /// No hidraw node matched the requested vid/pid/interface triple.
    ///
    /// The interface number matters: a Razer keyboard exposes several hidraw
    /// nodes and only one of them is the control interface.
    DeviceNotFound {
        /// USB vendor id that was searched for.
        vid: u16,
        /// USB product id that was searched for.
        pid: u16,
        /// USB interface number that was searched for (the device table's
        /// `report_index`).
        interface: u8,
    },
    /// The pid is not present in the `razer-devices` table.
    UnsupportedDevice {
        /// The unknown product id.
        pid: u16,
    },
    /// The device table says this model does not support that command.
    ///
    /// Returned *before* any bytes are sent — a capability guard that fires
    /// late is not a guard.
    Unsupported {
        /// The device the request was aimed at.
        pid: u16,
        /// What was asked for, e.g. `"poll rate"`.
        what: &'static str,
    },
    /// An `open`, `read` or `ioctl` failed, carrying the raw errno.
    Io {
        /// The operation that failed, e.g. `"HIDIOCSFEATURE"`.
        op: &'static str,
        /// The raw `errno` value.
        errno: i32,
    },
    /// `HIDIOCGFEATURE` handed back fewer than
    /// [`crate::HIDRAW_BUF_LEN`] bytes.
    ShortRead {
        /// How many bytes actually arrived.
        got: usize,
    },
    /// A sysfs attribute was missing, unreadable or not the shape expected.
    Sysfs(String),
    /// The device answered, but wrongly.
    Proto(ProtoError),
    /// All five attempts were used up.
    ///
    /// Mirrors the retry loop in `razer_send_payload()`
    /// (`razerkbd_driver.c:420-480`): five tries, 10 ms apart.
    RetriesExhausted {
        /// The protocol error from the final attempt.
        last: ProtoError,
    },
}

impl From<ProtoError> for HidError {
    fn from(e: ProtoError) -> Self {
        Self::Proto(e)
    }
}

impl core::fmt::Display for HidError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::DeviceNotFound {
                vid,
                pid,
                interface,
            } => write!(
                f,
                "no hidraw node for {vid:04x}:{pid:04x} on USB interface {interface}"
            ),
            Self::UnsupportedDevice { pid } => {
                write!(f, "product id {pid:#06x} is not a supported device")
            }
            Self::Unsupported { pid, what } => {
                write!(f, "device {pid:#06x} does not support {what}")
            }
            Self::Io { op, errno } => write!(f, "{op} failed: errno {errno}"),
            Self::ShortRead { got } => write!(
                f,
                "short feature read: got {got} bytes, expected {}",
                crate::transport::HIDRAW_BUF_LEN
            ),
            Self::Sysfs(msg) => write!(f, "sysfs: {msg}"),
            Self::Proto(e) => write!(f, "protocol error: {e}"),
            Self::RetriesExhausted { last } => {
                write!(f, "all 5 attempts failed, last error: {last}")
            }
        }
    }
}

impl std::error::Error for HidError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Proto(e) | Self::RetriesExhausted { last: e } => Some(e),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_is_non_empty_for_every_variant() {
        let variants = [
            HidError::DeviceNotFound {
                vid: 0x1532,
                pid: 0x028D,
                interface: 3,
            },
            HidError::UnsupportedDevice { pid: 0x0E05 },
            HidError::Unsupported {
                pid: 0x028D,
                what: "poll rate",
            },
            HidError::Io {
                op: "HIDIOCSFEATURE",
                errno: 13,
            },
            HidError::ShortRead { got: 0 },
            HidError::Sysfs("idVendor unreadable".into()),
            HidError::Proto(ProtoError::Malformed("x")),
            HidError::RetriesExhausted {
                last: ProtoError::Malformed("x"),
            },
        ];
        for v in &variants {
            assert!(!v.to_string().is_empty(), "empty Display for {v:?}");
        }
    }

    #[test]
    fn proto_errors_convert() {
        let e: HidError = ProtoError::Malformed("nope").into();
        assert!(matches!(e, HidError::Proto(ProtoError::Malformed("nope"))));
    }

    #[test]
    fn is_a_std_error_with_a_source_where_it_should_be() {
        fn assert_error<E: std::error::Error>(e: &E) -> Option<&(dyn std::error::Error + 'static)> {
            e.source()
        }
        assert!(
            assert_error(&HidError::Proto(ProtoError::Malformed("x"))).is_some(),
            "Proto should expose its inner error"
        );
        assert!(
            assert_error(&HidError::ShortRead { got: 1 }).is_none(),
            "ShortRead has no inner error"
        );
    }
}
