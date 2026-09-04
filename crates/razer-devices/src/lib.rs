// SPDX-License-Identifier: GPL-2.0-or-later
//! `razer-devices` — pure data + lookup for the Razer devices in scope.
//!
//! No I/O, no `unsafe`, no dependencies (not even `razer-proto`). Every value
//! transcribed here is cited against the upstream OpenRazer C driver source
//! (`the upstream OpenRazer `driver/` tree`) at the specific line that defines it.

#![forbid(unsafe_code)]

pub mod capabilities;
pub mod table;

pub use crate::capabilities::Capabilities;
pub use crate::table::{DeviceEntry, DeviceKind, PollRateKind, VENDOR_ID, all, lookup};
