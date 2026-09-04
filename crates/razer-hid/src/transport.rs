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

/// `HIDIOCGRAWINFO` — `_IOR('H', 0x03, struct hidraw_devinfo)`.
///
/// `(2 << 30) | (8 << 16) | (0x48 << 8) | 0x03`. Reads kernel-cached descriptor
/// data; **no packet reaches the device**.
const HIDIOCGRAWINFO: libc::c_ulong = 0x8008_4803;

/// `struct hidraw_devinfo` (`uapi/linux/hidraw.h`).
///
/// `vendor` and `product` are signed in the kernel header — a `u16` id above
/// `0x7FFF` arrives negative — so they are read as `i16` and cast, never
/// interpreted as signed.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
struct HidrawDevinfo {
    bustype: u32,
    vendor: i16,
    product: i16,
}

/// What `HIDIOCGRAWINFO` says a node is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawInfo {
    /// `BUS_USB` is `0x03`.
    pub bustype: u32,
    /// USB vendor id, as unsigned.
    pub vendor_id: u16,
    /// USB product id, as unsigned.
    pub product_id: u16,
}

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

    /// Open a hidraw node and confirm it is the device you meant.
    ///
    /// **Prefer this to [`open`](HidrawDevice::open) on any path that will send
    /// a report.** Enumeration resolves a name through sysfs; this opens that
    /// name later, and `/dev/hidrawN` minor numbers are *reused*. If the device
    /// re-enumerates in between — which this project deliberately provokes, and
    /// which `tools/usb_replug.py` exists to trigger — the same node number can
    /// belong to an entirely different device, and Razer feature reports would
    /// then be written into whatever landed there.
    ///
    /// `HIDIOCGRAWINFO` closes that window: it reads kernel-cached descriptor
    /// data for the fd we actually hold, so it answers "what did I just open?"
    /// rather than "what was at this path a moment ago". It sends nothing to the
    /// device — it is exactly rung 0 of `tools/validate_hidraw.py`.
    ///
    /// # Errors
    ///
    /// [`HidError::Io`] if the open or the ioctl fails;
    /// [`HidError::DeviceIdentityMismatch`] if the node is not `vid:pid`.
    pub fn open_expecting(
        path: &Path,
        vid: u16,
        pid: u16,
        opt_in: &HardwareOptIn,
    ) -> Result<Self, HidError> {
        let dev = Self::open(path, opt_in)?;
        let info = dev.raw_info()?;
        if info.vendor_id != vid || info.product_id != pid {
            return Err(HidError::DeviceIdentityMismatch {
                path: dev.path.clone(),
                expected: (vid, pid),
                got: (info.vendor_id, info.product_id),
            });
        }
        Ok(dev)
    }

    /// Ask the kernel what this fd is, via `HIDIOCGRAWINFO`.
    ///
    /// Reads cached descriptor data only; no packet reaches the device.
    ///
    /// # Errors
    ///
    /// [`HidError::Io`] with `op = "HIDIOCGRAWINFO"` if the ioctl fails.
    pub fn raw_info(&self) -> Result<RawInfo, HidError> {
        let mut info = HidrawDevinfo::default();
        // HIDIOCGRAWINFO writes exactly one `struct hidraw_devinfo` into the
        // pointed-at buffer; `info` is a local `#[repr(C)]` value of that type,
        // uniquely borrowed, and `self.file` owns a live fd for the call.
        // SAFETY: valid, uniquely-borrowed, correctly-typed pointer; live fd.
        let rc = unsafe {
            libc::ioctl(
                self.file.as_raw_fd(),
                HIDIOCGRAWINFO,
                std::ptr::from_mut(&mut info).cast::<libc::c_void>(),
            )
        };
        if rc < 0 {
            return Err(HidError::Io {
                op: "HIDIOCGRAWINFO",
                errno: last_errno(),
            });
        }
        Ok(RawInfo {
            bustype: info.bustype,
            // The kernel's fields are __s16; ids above 0x7FFF arrive negative.
            vendor_id: info.vendor as u16,
            product_id: info.product as u16,
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

    /// `_IOR(type, nr, size)` — direction `_IOC_READ = 2`, for HIDIOCGRAWINFO.
    fn ior(nr: u32, size: u32) -> libc::c_ulong {
        libc::c_ulong::from((2u32 << 30) | (size << 16) | (0x48u32 << 8) | nr)
    }

    #[test]
    fn rawinfo_request_number_and_struct_layout_match_the_kernel() {
        // struct hidraw_devinfo { __u32 bustype; __s16 vendor; __s16 product; }
        assert_eq!(core::mem::size_of::<HidrawDevinfo>(), 8);
        assert_eq!(core::mem::align_of::<HidrawDevinfo>(), 4);
        assert_eq!(HIDIOCGRAWINFO, ior(0x03, 8));
        assert_eq!(HIDIOCGRAWINFO, 0x8008_4803);
    }

    #[test]
    fn product_ids_above_0x7fff_survive_the_kernels_signed_fields() {
        // The kernel declares vendor/product as __s16, so 0xC52B arrives as a
        // negative i16. Reading them as signed would make a legitimate device
        // fail the identity check in `open_expecting`.
        let info = HidrawDevinfo {
            bustype: 0x03,
            vendor: 0x1532,
            product: 0xC52Bu16 as i16,
        };
        assert!(
            info.product < 0,
            "the test premise: this is negative as i16"
        );
        assert_eq!(info.product as u16, 0xC52B);
        assert_eq!(info.vendor as u16, 0x1532);
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
