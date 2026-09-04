// SPDX-License-Identifier: GPL-2.0-or-later
//! The feature-report transport: the trait, the opt-in token, and the real
//! `hidraw` implementation.
//!
//! # Why feature reports
//!
//! `razer_send_control_msg()` (`razercommon.c:20-43`) issues a USB control
//! transfer with `bRequest = HID_REQ_SET_REPORT (0x09)`,
//! `bmRequestType = 0x21` and `wValue = 0x0300` — report type 3 (FEATURE),
//! report id 0. `razer_get_usb_response()` (`razercommon.c:63-100`) does the
//! matching `HID_REQ_GET_REPORT (0x01)` with `bmRequestType = 0xA1`.
//!
//! Over `hidraw` those two are `HIDIOCSFEATURE` and `HIDIOCGFEATURE`. The
//! kernel supplies `wIndex` itself, from the USB interface that owns the node —
//! which is why picking the right node is the whole of [`crate::enumerate`].

use core::fmt;
use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use crate::error::HidError;

/// One report-id byte plus the 90-byte `struct razer_report`.
///
/// Report id 0 is still prefixed even though these devices use no report ids —
/// that is the `hidraw` convention, and it matches the low byte of the C
/// driver's `wValue = 0x0300`.
pub const HIDRAW_BUF_LEN: usize = 91;

/// `HIDIOCSFEATURE(91)` — `_IOC(_IOC_WRITE|_IOC_READ, 'H', 0x06, 91)`.
///
/// `(3 << 30) | (91 << 16) | (0x48 << 8) | 0x06`.
const HIDIOCSFEATURE_91: libc::c_ulong = 0xC05B_4806;

/// `HIDIOCGFEATURE(91)` — `_IOC(_IOC_WRITE|_IOC_READ, 'H', 0x07, 91)`.
const HIDIOCGFEATURE_91: libc::c_ulong = 0xC05B_4807;

/// Proof that the caller has explicitly asked to touch real hardware.
///
/// This type is the safety interlock for the whole workspace. It holds a
/// private unit field, so it cannot be constructed by a struct literal; it has
/// no `Default`, no `Clone` and no derive that would conjure one. The single
/// constructor is
/// [`i_understand_this_touches_real_hardware`](HardwareOptIn::i_understand_this_touches_real_hardware),
/// named so that every call site shows up in a grep.
///
/// Nothing in this crate's tests holds one, which is what makes
/// `cargo test -p razer-hid` provably incapable of reaching a device.
#[derive(Debug)]
pub struct HardwareOptIn(());

impl HardwareOptIn {
    /// The one and only way to obtain a [`HardwareOptIn`].
    ///
    /// Calling this is a statement that a real Razer device is about to be
    /// opened and written to. On the BlackWidow V4 Pro that can, in the failure
    /// mode this project exists to study, drop the keyboard off the USB bus —
    /// so a human should be at the desk when it happens.
    ///
    /// ```no_run
    /// use razer_hid::{HardwareOptIn, HidrawDevice};
    /// use std::path::Path;
    ///
    /// let opt_in = HardwareOptIn::i_understand_this_touches_real_hardware();
    /// let dev = HidrawDevice::open(Path::new("/dev/hidraw3"), &opt_in)?;
    /// println!("opened {}", dev.path().display());
    /// # Ok::<(), razer_hid::HidError>(())
    /// ```
    #[must_use]
    pub fn i_understand_this_touches_real_hardware() -> Self {
        Self(())
    }
}

/// The transport contract.
///
/// Buffers are always exactly [`HIDRAW_BUF_LEN`] bytes, with `buf[0] = 0x00`
/// (the report id) and `buf[1..]` the 90-byte report.
pub trait FeatureTransport {
    /// Issue a `SET_REPORT` (feature) carrying `buf`.
    ///
    /// # Errors
    ///
    /// [`HidError::Io`] with `op = "HIDIOCSFEATURE"` if the ioctl fails.
    fn set_feature(&mut self, buf: &[u8; HIDRAW_BUF_LEN]) -> Result<(), HidError>;

    /// Issue a `GET_REPORT` (feature) into `buf`, returning the byte count the
    /// device actually produced.
    ///
    /// A count below [`HIDRAW_BUF_LEN`] is not an error at this layer — the
    /// caller decides, and [`crate::Session`] turns it into
    /// [`HidError::ShortRead`].
    ///
    /// # Errors
    ///
    /// [`HidError::Io`] with `op = "HIDIOCGFEATURE"` if the ioctl fails.
    fn get_feature(&mut self, buf: &mut [u8; HIDRAW_BUF_LEN]) -> Result<usize, HidError>;
}

/// A real `/dev/hidraw*` node, opened `O_RDWR`.
///
/// Constructible only via [`HidrawDevice::open`], which demands a
/// [`HardwareOptIn`].
pub struct HidrawDevice {
    file: File,
    path: PathBuf,
}

impl fmt::Debug for HidrawDevice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HidrawDevice")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl HidrawDevice {
    /// Open a hidraw node for reading and writing.
    ///
    /// This is one of only three functions in the crate that touches the
    /// filesystem, and the only one that opens a device node.
    ///
    /// # Errors
    ///
    /// [`HidError::Io`] with `op = "open"` if the node cannot be opened —
    /// `ENOENT` if it has gone away, `EACCES` if the calling user is not in the
    /// group the udev rule grants.
    pub fn open(path: &Path, _opt_in: &HardwareOptIn) -> Result<Self, HidError> {
        let file = File::options()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| HidError::Io {
                op: "open",
                errno: e.raw_os_error().unwrap_or(0),
            })?;
        Ok(Self {
            file,
            path: path.to_path_buf(),
        })
    }

    /// The node this device was opened from.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl FeatureTransport for HidrawDevice {
    fn set_feature(&mut self, buf: &[u8; HIDRAW_BUF_LEN]) -> Result<(), HidError> {
        // HIDIOCSFEATURE is an `_IOC_WRITE | _IOC_READ` ioctl, so the kernel is
        // entitled to write back into the buffer. Copy rather than hand it the
        // caller's read-only report.
        let mut scratch = *buf;
        // HIDIOCSFEATURE(91) takes a pointer to a caller-owned buffer of exactly
        // 91 bytes, which is the length encoded in the request number. `scratch`
        // is a local `[u8; 91]` that outlives the call and is not aliased, and
        // `self.file` owns a live fd for the duration of the borrow.
        // SAFETY: valid, uniquely-owned, correctly-sized pointer; live fd.
        let rc = unsafe {
            libc::ioctl(
                self.file.as_raw_fd(),
                HIDIOCSFEATURE_91,
                scratch.as_mut_ptr().cast::<libc::c_void>(),
            )
        };
        if rc < 0 {
            return Err(HidError::Io {
                op: "HIDIOCSFEATURE",
                errno: last_errno(),
            });
        }
        Ok(())
    }

    fn get_feature(&mut self, buf: &mut [u8; HIDRAW_BUF_LEN]) -> Result<usize, HidError> {
        // hidraw takes the report number to fetch in byte 0.
        buf[0] = 0x00;
        // HIDIOCGFEATURE(91) writes at most 91 bytes into the pointed-at buffer.
        // Identical reasoning to `set_feature` above: `buf` is a
        // uniquely-borrowed `&mut [u8; 91]`, correctly sized for the request
        // number, and `self.file` owns a live fd for the duration of the call.
        // SAFETY: valid, uniquely-borrowed, correctly-sized pointer; live fd.
        let rc = unsafe {
            libc::ioctl(
                self.file.as_raw_fd(),
                HIDIOCGFEATURE_91,
                buf.as_mut_ptr().cast::<libc::c_void>(),
            )
        };
        if rc < 0 {
            return Err(HidError::Io {
                op: "HIDIOCGFEATURE",
                errno: last_errno(),
            });
        }
        Ok(rc as usize)
    }
}

impl Drop for HidrawDevice {
    /// Closes the fd.
    ///
    /// The `File` field owns the descriptor, so dropping it issues the
    /// `close(2)`. This impl exists to make that ordering explicit and to give
    /// the close a single, obvious home.
    fn drop(&mut self) {
        // `self.file` is dropped here, closing the descriptor.
    }
}

/// The current thread's `errno`, or 0 if the platform declines to say.
fn last_errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Recompute the ioctl request numbers from the kernel's `_IOC` macro so a
    /// typo in the constants above cannot survive.
    ///
    /// `_IOC(dir, type, nr, size) = (dir << 30) | (size << 16) | (type << 8) | nr`
    /// with `dir = _IOC_WRITE | _IOC_READ = 3`, `type = 'H' = 0x48`.
    fn ioc(nr: u32, size: u32) -> libc::c_ulong {
        libc::c_ulong::from((3u32 << 30) | (size << 16) | (0x48u32 << 8) | nr)
    }

    #[test]
    fn buffer_is_one_report_id_plus_ninety_bytes() {
        // razercommon.h:136 — `static_assert(sizeof(struct razer_report) == 90)`.
        assert_eq!(HIDRAW_BUF_LEN, 1 + 90);
        assert_eq!(HIDRAW_BUF_LEN, 91);
    }

    #[test]
    fn ioctl_request_numbers_match_the_ioc_macro() {
        assert_eq!(HIDIOCSFEATURE_91, ioc(0x06, HIDRAW_BUF_LEN as u32));
        assert_eq!(HIDIOCGFEATURE_91, ioc(0x07, HIDRAW_BUF_LEN as u32));
        // And the literals the frozen API quotes.
        assert_eq!(HIDIOCSFEATURE_91, 0xC05B_4806);
        assert_eq!(HIDIOCGFEATURE_91, 0xC05B_4807);
    }

    #[test]
    fn the_opt_in_token_is_zero_sized_and_field_private() {
        // If a struct literal ever becomes possible outside this module the
        // interlock is gone; the private unit field is what prevents it.
        assert_eq!(core::mem::size_of::<HardwareOptIn>(), 0);
    }
}
