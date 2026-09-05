// SPDX-License-Identifier: GPL-2.0-or-later
//! The error type for enumeration and transport.

use razer_proto::{ProtoError, report::Status};

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
    /// The opened node is not the device that was enumerated.
    ///
    /// hidraw minor numbers are reused, so a device that re-enumerates between
    /// [`crate::find_device`] and [`crate::HidrawDevice::open_expecting`] can
    /// leave a *different* device at the same `/dev/hidrawN`. Raised by the
    /// post-open `HIDIOCGRAWINFO` check, before any report is sent.
    DeviceIdentityMismatch {
        /// The node that was opened.
        path: std::path::PathBuf,
        /// The `(vid, pid)` enumeration promised.
        expected: (u16, u16),
        /// The `(vid, pid)` the kernel reports for the fd actually held.
        got: (u16, u16),
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

impl HidError {
    /// The device's own terminal status byte, when there was one.
    ///
    /// The C driver's `razer_send_payload()` ends its retry loop with a switch
    /// that gives each terminal status its own errno
    /// (`razermouse_driver.c:157-175`):
    ///
    /// ```text
    /// RAZER_CMD_FAILURE       -> -EINVAL
    /// RAZER_CMD_NOT_SUPPORTED -> -ENOTSUPP
    /// RAZER_CMD_TIMEOUT       -> -ETIMEDOUT
    /// ```
    ///
    /// [`HidError::RetriesExhausted`] collapses all three into one variant, so
    /// this restores the distinction callers need without changing the retry
    /// policy itself — which stays faithful to upstream because
    /// `docs/phase3-experiment.md` measures it.
    ///
    /// Returns `None` for transport faults, which never carry a device status.
    #[must_use]
    pub fn terminal_status(&self) -> Option<Status> {
        match self {
            Self::RetriesExhausted {
                last: ProtoError::DeviceStatus(status),
            }
            | Self::Proto(ProtoError::DeviceStatus(status)) => Some(*status),
            _ => None,
        }
    }

    /// True when the device answered, consistently, with `RAZER_CMD_TIMEOUT`
    /// (`0x04`).
    ///
    /// This is what a Razer wireless receiver says when it is plugged in but
    /// has no mouse behind it — the normal state whenever the mouse is on its
    /// cable, because the dongle stays enumerated alongside the wired device.
    /// It is a definite answer from a working dongle, not a fault: the receiver
    /// is telling us it has nobody to relay to.
    ///
    /// Report it as a device *state*, not an error. Observed on a Basilisk V3
    /// Pro on 2026-09-05 with the mouse wired: `0x00AA` read firmware v2.0
    /// while `0x00AB`, the idle dongle, returned `0x04` to every request.
    #[must_use]
    pub fn is_receiver_idle(&self) -> bool {
        self.terminal_status() == Some(Status::Timeout)
    }
}

#[cfg(test)]
mod terminal_status_tests {
    use super::*;

    #[test]
    fn an_idle_receiver_is_recognised_from_a_timeout_status() {
        let e = HidError::RetriesExhausted {
            last: ProtoError::DeviceStatus(Status::Timeout),
        };
        assert_eq!(e.terminal_status(), Some(Status::Timeout));
        assert!(e.is_receiver_idle());
    }

    #[test]
    fn the_other_terminal_statuses_are_kept_distinct() {
        // Upstream gives each of these its own errno; so must we.
        for status in [Status::Failure, Status::NotSupported] {
            let e = HidError::RetriesExhausted {
                last: ProtoError::DeviceStatus(status),
            };
            assert_eq!(e.terminal_status(), Some(status));
            assert!(
                !e.is_receiver_idle(),
                "{status:?} must not be mistaken for an idle receiver"
            );
        }
    }

    #[test]
    fn a_transport_fault_carries_no_device_status() {
        // An EACCES is not the device saying anything at all.
        let e = HidError::Io {
            op: "HIDIOCSFEATURE",
            errno: 13,
        };
        assert_eq!(e.terminal_status(), None);
        assert!(!e.is_receiver_idle());
    }

    #[test]
    fn a_non_status_protocol_error_is_not_an_idle_receiver() {
        let e = HidError::RetriesExhausted {
            last: ProtoError::BadCrc {
                expected: 0x05,
                got: 0x06,
            },
        };
        assert_eq!(e.terminal_status(), None);
        assert!(!e.is_receiver_idle());
    }
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
            Self::DeviceIdentityMismatch {
                path,
                expected,
                got,
            } => write!(
                f,
                "{} is {:04x}:{:04x}, not the {:04x}:{:04x} that was enumerated \
                 (the device re-enumerated and the node number was reused)",
                path.display(),
                got.0,
                got.1,
                expected.0,
                expected.1
            ),
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
            HidError::DeviceIdentityMismatch {
                path: "/dev/hidraw3".into(),
                expected: (0x1532, 0x028D),
                got: (0x046D, 0xC52B),
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
