// SPDX-License-Identifier: GPL-2.0-or-later
//! Reading USB interface metadata for a `hidraw` node out of `/sys`.
//!
//! This module reads **filesystem metadata only** — attribute files under
//! `/sys`. It never opens a `/dev/hidraw*` node; that is
//! [`crate::transport::HidrawDevice`]'s job.
//!
//! # The layout being walked
//!
//! ```text
//! /sys/class/hidraw/hidraw3/device -> .../1-2.3:1.3/0003:1532:028D.0004
//!                                          |         |
//!                                          |         `- the HID device
//!                                          `- the USB interface: bInterfaceNumber,
//!                                             bInterfaceProtocol
//!        its parent (.../1-2.3) is the USB device: idVendor, idProduct
//! ```
//!
//! So: resolve `device`, walk up to the first ancestor carrying
//! `bInterfaceNumber`, and take `idVendor`/`idProduct` from that ancestor's
//! parent.

use std::path::{Path, PathBuf};

use crate::error::HidError;
use crate::transport::HardwareOptIn;

/// How far up the sysfs tree to look for the USB interface directory before
/// concluding the node has no USB ancestor.
///
/// A hidraw node sits at most a handful of levels below its interface; twelve
/// is generous and keeps a symlink loop from turning into a hang.
const MAX_ANCESTOR_WALK: usize = 12;

/// What a hidraw node's parent USB interface says about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbInterfaceInfo {
    /// `idVendor` of the USB device — `0x1532` for Razer.
    pub vendor_id: u16,
    /// `idProduct` of the USB device.
    pub product_id: u16,
    /// `bInterfaceNumber` of the interface owning this hidraw node.
    ///
    /// This is the value that must equal the device table's `report_index`:
    /// the C driver's report index is the `wIndex` of a `USB_RECIP_INTERFACE`
    /// control transfer, and that *is* the interface number.
    pub interface_number: u8,
    /// `bInterfaceProtocol`. `0x02` is `USB_INTERFACE_PROTOCOL_MOUSE`, which on
    /// both devices in scope marks the control interface.
    pub interface_protocol: u8,
}

/// Abstracts the sysfs walk so enumeration can be driven from memory.
///
/// The real implementation is [`RealSysfs`]; every test in this crate uses
/// [`crate::MockSysfs`].
pub trait SysfsSource {
    /// The entry names under `/sys/class/hidraw`, e.g. `["hidraw0", "hidraw3"]`.
    ///
    /// Order is whatever the source produces; [`crate::find_device`] sorts
    /// before searching, so callers get a deterministic answer regardless.
    ///
    /// # Errors
    ///
    /// [`HidError::Io`] if the directory cannot be listed.
    fn hidraw_names(&self) -> Result<Vec<String>, HidError>;

    /// Resolve the parent USB interface for one hidraw node.
    ///
    /// Returns `Ok(None)` when the node has no USB interface ancestor — a
    /// Bluetooth, I2C or virtual HID device. That is a normal condition, not an
    /// error: such nodes are simply skipped.
    ///
    /// # Errors
    ///
    /// [`HidError::Sysfs`] if an attribute exists but does not parse.
    fn interface_info(&self, hidraw_name: &str) -> Result<Option<UsbInterfaceInfo>, HidError>;

    /// The device node path for a hidraw name, e.g. `/dev/hidraw3`.
    fn device_path(&self, hidraw_name: &str) -> PathBuf;
}

/// The real sysfs walker.
///
/// Requires a [`HardwareOptIn`] because it reads the live `/sys` tree. It still
/// opens no device node — but it is the step immediately before doing so, and
/// the token is what makes "did anything in this run go near the hardware?"
/// answerable by grep.
#[derive(Debug)]
pub struct RealSysfs {
    root: PathBuf,
}

impl RealSysfs {
    /// A walker rooted at `/sys`.
    pub fn new(opt_in: &HardwareOptIn) -> Self {
        Self::with_root(opt_in, PathBuf::from("/sys"))
    }

    /// A walker rooted somewhere else.
    ///
    /// A diagnostic hook — point it at a captured copy of a `/sys` tree to
    /// reproduce an enumeration problem offline. Note that
    /// [`SysfsSource::device_path`] still returns `/dev/<name>`: the root
    /// affects where metadata is *read*, not where the device node lives.
    pub fn with_root(_opt_in: &HardwareOptIn, root: PathBuf) -> Self {
        Self { root }
    }

    /// `<root>/class/hidraw`.
    fn class_dir(&self) -> PathBuf {
        self.root.join("class").join("hidraw")
    }
}

impl SysfsSource for RealSysfs {
    fn hidraw_names(&self) -> Result<Vec<String>, HidError> {
        let dir = self.class_dir();
        let entries = std::fs::read_dir(&dir).map_err(|e| HidError::Io {
            op: "read_dir(/sys/class/hidraw)",
            errno: e.raw_os_error().unwrap_or(0),
        })?;
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| HidError::Io {
                op: "read_dir(/sys/class/hidraw)",
                errno: e.raw_os_error().unwrap_or(0),
            })?;
            if let Some(name) = entry.file_name().to_str() {
                names.push(name.to_owned());
            }
        }
        Ok(names)
    }

    fn interface_info(&self, hidraw_name: &str) -> Result<Option<UsbInterfaceInfo>, HidError> {
        let link = self.class_dir().join(hidraw_name).join("device");
        // A node that has just been unplugged, or one whose `device` link does
        // not resolve, is not an error — it is simply not our device.
        let Ok(resolved) = std::fs::canonicalize(&link) else {
            return Ok(None);
        };

        let Some(iface) = resolved
            .ancestors()
            .take(MAX_ANCESTOR_WALK)
            .find(|dir| dir.join("bInterfaceNumber").is_file())
        else {
            return Ok(None);
        };
        let Some(usb) = iface.parent() else {
            return Ok(None);
        };
        if !usb.join("idVendor").is_file() {
            return Ok(None);
        }

        Ok(Some(UsbInterfaceInfo {
            vendor_id: read_hex(&usb.join("idVendor"))?,
            product_id: read_hex(&usb.join("idProduct"))?,
            interface_number: read_hex(&iface.join("bInterfaceNumber"))?,
            interface_protocol: read_hex(&iface.join("bInterfaceProtocol"))?,
        }))
    }

    fn device_path(&self, hidraw_name: &str) -> PathBuf {
        PathBuf::from("/dev").join(hidraw_name)
    }
}

/// Read a sysfs attribute holding a hexadecimal integer.
///
/// USB attributes are printed without a `0x` prefix — `idVendor` is `"1532"`,
/// `bInterfaceNumber` is `"03"`.
fn read_hex<T: TryFrom<u32>>(path: &Path) -> Result<T, HidError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| HidError::Sysfs(format!("{}: {e}", path.display())))?;
    let text = raw.trim();
    let value = u32::from_str_radix(text, 16)
        .map_err(|e| HidError::Sysfs(format!("{}: {text:?} is not hex: {e}", path.display())))?;
    T::try_from(value).map_err(|_| {
        HidError::Sysfs(format!(
            "{}: {value:#x} does not fit the expected width",
            path.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interface_info_is_comparable_by_value() {
        let a = UsbInterfaceInfo {
            vendor_id: 0x1532,
            product_id: 0x028D,
            interface_number: 3,
            interface_protocol: 0x02,
        };
        let b = a.clone();
        assert_eq!(a, b);
        let c = UsbInterfaceInfo {
            interface_number: 0,
            ..a.clone()
        };
        assert_ne!(a, c);
    }

    #[test]
    fn ancestor_walk_is_bounded() {
        // A symlink loop under /sys must not turn into a hang; the bound is what
        // guarantees that, so pin it.
        const { assert!(MAX_ANCESTOR_WALK > 0 && MAX_ANCESTOR_WALK <= 32) };

        // The bound must also be generous enough to reach a real USB interface.
        // This is the BlackWidow V4 Pro's actual sysfs path shape; `ancestors`
        // yields the path itself first, and the interface directory
        // (`1-2.3:1.3`) is the second entry. Anything under the bound is fine —
        // the assertion is that a real device is nowhere near it.
        let hid_device = Path::new(
            "/sys/devices/pci0000:00/0000:00:14.0/usb1/1-2/1-2.3/1-2.3:1.3/0003:1532:028D.0004",
        );
        let depth_to_interface = hid_device
            .ancestors()
            .position(|p| p.file_name().is_some_and(|n| n == "1-2.3:1.3"))
            .expect("the interface directory is an ancestor");
        assert_eq!(depth_to_interface, 1, "the interface is the direct parent");
        assert!(
            hid_device.ancestors().count() <= MAX_ANCESTOR_WALK,
            "a real sysfs path must fit inside the bound with room to spare"
        );
    }
}
