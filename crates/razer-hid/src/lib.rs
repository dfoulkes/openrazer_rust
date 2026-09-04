// SPDX-License-Identifier: GPL-2.0-or-later
//! `razer-hid` — hidraw enumeration and the Razer feature-report transport.
//!
//! This crate is the only one in the workspace that can touch hardware, and it
//! is built so that it cannot do so by accident.
//!
//! # The safety contract
//!
//! Every code path capable of reaching a real device — reading `/sys`, opening
//! `/dev/hidraw*`, issuing an ioctl — requires a [`HardwareOptIn`] token, and
//! the only way to obtain one is that type's single, deliberately verbose
//! constructor, whose name states in full what calling it does. There is no
//! `Default` impl for it, nothing derives one, and no test in this crate calls
//! it — the name appears nowhere in this crate but its own definition, which is
//! checkable with one `grep`. Everything else in here is driven from in-memory
//! fixtures ([`MockSysfs`], [`MockTransport`], [`MockClock`]).
//!
//! # Layering
//!
//! - [`sysfs`] — reads USB interface metadata for a hidraw node, behind the
//!   [`SysfsSource`] trait so it can be faked.
//! - [`enumerate`] — picks the right `/dev/hidraw*` node for a device. The
//!   selection key is the **USB interface number**, because the C driver's
//!   "report index" is the `wIndex` of a `USB_RECIP_INTERFACE` control
//!   transfer, which *is* the interface number.
//! - [`transport`] — the [`FeatureTransport`] trait and its real
//!   `HIDIOCSFEATURE` / `HIDIOCGFEATURE` implementation.
//! - [`session`] — binds a `razer_devices::DeviceEntry` to a transport. This is
//!   the one and only place in the codebase that writes the transaction id
//!   byte, which is precisely the bug this project exists to fix
//!   (`razerkbd_driver.c:4308` hardcodes `0xFF` where the BlackWidow V4 Pro
//!   needs `0x1F`).

#![deny(missing_docs)]
#![deny(unsafe_op_in_unsafe_fn)]

pub mod enumerate;
pub mod error;
pub mod mock;
pub mod session;
pub mod sysfs;
pub mod transport;

pub use crate::enumerate::{HidrawNode, find_device, list_razer_nodes};
pub use crate::error::HidError;
pub use crate::mock::{MockClock, MockSysfs, MockTransport};
pub use crate::session::{Clock, RealClock, Session};
pub use crate::sysfs::{RealSysfs, SysfsSource, UsbInterfaceInfo};
pub use crate::transport::{
    FeatureTransport, HIDRAW_BUF_LEN, HardwareOptIn, HidrawDevice, RawInfo,
};
