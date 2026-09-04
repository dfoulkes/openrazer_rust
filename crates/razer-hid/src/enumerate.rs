// SPDX-License-Identifier: GPL-2.0-or-later
//! Picking the right `/dev/hidraw*` node.
//!
//! # Why the interface number is the whole story
//!
//! `razer_send_control_msg()` (`razercommon.c:20-43`) sends its control
//! transfer with `bmRequestType` including `USB_RECIP_INTERFACE`, and passes
//! the driver's per-device *report index* as `wIndex`. For a
//! recipient-interface transfer, `wIndex` **is** the USB interface number.
//!
//! Over `hidraw` we cannot choose `wIndex` — the kernel fills it in from the
//! interface that owns the node we opened. So the only faithful way to
//! reproduce the C driver's behaviour is to open the hidraw node whose parent
//! USB interface number equals the device table's `report_index`. That is what
//! [`find_device`] does, and getting it wrong means talking to a keyboard's
//! media-key interface and wondering why nothing answers.

use std::path::PathBuf;

use crate::error::HidError;
use crate::sysfs::{SysfsSource, UsbInterfaceInfo};

/// One `hidraw` node and what its parent USB interface says about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HidrawNode {
    /// The sysfs class entry name, e.g. `"hidraw3"`.
    pub name: String,
    /// The device node, e.g. `/dev/hidraw3`.
    pub path: PathBuf,
    /// The parent USB interface's identity.
    pub info: UsbInterfaceInfo,
}

/// The numeric suffix of a `hidrawN` name, used to order enumeration.
///
/// Anything that does not fit the pattern sorts last, by name, so an
/// unrecognised entry can never displace a real one.
fn hidraw_index(name: &str) -> Option<u32> {
    name.strip_prefix("hidraw")?.parse().ok()
}

/// Sort names into ascending `hidrawN` order.
fn sort_by_index(names: &mut [String]) {
    names.sort_by(|a, b| {
        let ka = (hidraw_index(a).unwrap_or(u32::MAX), a.as_str());
        let kb = (hidraw_index(b).unwrap_or(u32::MAX), b.as_str());
        ka.cmp(&kb)
    });
}

/// Find the control node for a device.
///
/// Matches on all three of `vendor_id == vid`, `product_id == pid` and
/// `interface_number == interface`. `interface` **must** come from
/// `razer_devices::DeviceEntry::report_index` — see the module docs for why
/// those are the same number.
///
/// Nodes are considered in ascending `hidrawN` order regardless of the order
/// the [`SysfsSource`] lists them in, and the first match wins. The result is
/// therefore deterministic across runs even though `read_dir` order is not.
/// Nodes with no USB parent (Bluetooth, virtual) are skipped, not treated as
/// errors.
///
/// # Errors
///
/// [`HidError::DeviceNotFound`] if nothing matches; whatever the
/// [`SysfsSource`] returns if the walk itself fails.
pub fn find_device<S: SysfsSource + ?Sized>(
    sysfs: &S,
    vid: u16,
    pid: u16,
    interface: u8,
) -> Result<HidrawNode, HidError> {
    let mut names = sysfs.hidraw_names()?;
    sort_by_index(&mut names);

    for name in names {
        let Some(info) = sysfs.interface_info(&name)? else {
            continue;
        };
        if info.vendor_id == vid && info.product_id == pid && info.interface_number == interface {
            return Ok(HidrawNode {
                path: sysfs.device_path(&name),
                name,
                info,
            });
        }
    }

    Err(HidError::DeviceNotFound {
        vid,
        pid,
        interface,
    })
}

/// Every Razer-vendor hidraw node present, on any interface.
///
/// For `--list` style output and for diagnosing "which node is which" by hand.
/// Returned in ascending `hidrawN` order. Devices absent from the
/// `razer-devices` table are still listed — the point is to show what is
/// actually plugged in.
///
/// # Errors
///
/// Whatever the [`SysfsSource`] returns if the walk fails.
pub fn list_razer_nodes<S: SysfsSource + ?Sized>(sysfs: &S) -> Result<Vec<HidrawNode>, HidError> {
    let mut names = sysfs.hidraw_names()?;
    sort_by_index(&mut names);

    let mut out = Vec::new();
    for name in names {
        let Some(info) = sysfs.interface_info(&name)? else {
            continue;
        };
        if info.vendor_id == razer_devices::VENDOR_ID {
            out.push(HidrawNode {
                path: sysfs.device_path(&name),
                name,
                info,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockSysfs;

    /// `USB_INTERFACE_PROTOCOL_MOUSE` — the control interface on both devices
    /// in scope.
    const PROTO_CONTROL: u8 = 0x02;

    const RAZER: u16 = 0x1532;
    const BLACKWIDOW_V4_PRO: u16 = 0x028D;
    const BASILISK_V3_PRO_WIRELESS: u16 = 0x00AB;
    const BASILISK_V3_PRO_WIRED: u16 = 0x00AA;

    fn info(pid: u16, iface: u8, proto: u8) -> UsbInterfaceInfo {
        UsbInterfaceInfo {
            vendor_id: RAZER,
            product_id: pid,
            interface_number: iface,
            interface_protocol: proto,
        }
    }

    /// The wired Basilisk shares the wireless model's interface layout — the
    /// two pids are adjacent case labels in every switch in
    /// `razermouse_driver.c`.
    fn basilisk_v3_pro_wired() -> MockSysfs {
        MockSysfs::new()
            .with_node("hidraw4", info(BASILISK_V3_PRO_WIRED, 0, PROTO_CONTROL))
            .with_node("hidraw5", info(BASILISK_V3_PRO_WIRED, 1, 0x01))
            .with_node("hidraw6", info(BASILISK_V3_PRO_WIRED, 2, 0x00))
    }

    fn fixture_for(pid: u16) -> MockSysfs {
        match pid {
            BLACKWIDOW_V4_PRO => MockSysfs::blackwidow_v4_pro(),
            BASILISK_V3_PRO_WIRELESS => MockSysfs::basilisk_v3_pro_wireless(),
            BASILISK_V3_PRO_WIRED => basilisk_v3_pro_wired(),
            other => panic!("no fixture for pid {other:#06x}"),
        }
    }

    // --- criterion 6 ---
    #[test]
    fn blackwidow_control_node_is_hidraw3_on_interface_3() {
        let node = find_device(&MockSysfs::blackwidow_v4_pro(), RAZER, BLACKWIDOW_V4_PRO, 3)
            .expect("the fixture has an interface 3");
        assert_eq!(node.path, PathBuf::from("/dev/hidraw3"));
        assert_eq!(node.name, "hidraw3");
        assert_eq!(node.info.interface_number, 3);
    }

    // --- criterion 7 ---
    #[test]
    fn selection_is_by_interface_number_not_first_razer_node() {
        let node = find_device(&MockSysfs::blackwidow_v4_pro(), RAZER, BLACKWIDOW_V4_PRO, 0)
            .expect("the fixture has an interface 0");
        assert_eq!(
            node.path,
            PathBuf::from("/dev/hidraw0"),
            "asking for interface 0 must not return the control node"
        );
    }

    // --- criterion 8 ---
    #[test]
    fn basilisk_wireless_control_node_is_hidraw4_on_interface_0() {
        let node = find_device(
            &MockSysfs::basilisk_v3_pro_wireless(),
            RAZER,
            BASILISK_V3_PRO_WIRELESS,
            0,
        )
        .expect("the fixture has an interface 0");
        assert_eq!(node.path, PathBuf::from("/dev/hidraw4"));
        assert_eq!(node.info.interface_protocol, PROTO_CONTROL);
    }

    // --- criterion 9 ---
    #[test]
    fn absent_pid_is_device_not_found() {
        let err = find_device(&MockSysfs::blackwidow_v4_pro(), RAZER, 0x0E05, 3)
            .expect_err("the Kiyo Pro is not in the fixture, and never will be");
        match err {
            HidError::DeviceNotFound {
                vid,
                pid,
                interface,
            } => {
                assert_eq!((vid, pid, interface), (RAZER, 0x0E05, 3));
            }
            other => panic!("expected DeviceNotFound, got {other:?}"),
        }
    }

    #[test]
    fn absent_interface_on_a_present_pid_is_device_not_found() {
        let err = find_device(&MockSysfs::blackwidow_v4_pro(), RAZER, BLACKWIDOW_V4_PRO, 7)
            .expect_err("the fixture has no interface 7");
        assert!(matches!(err, HidError::DeviceNotFound { interface: 7, .. }));
    }

    // --- criterion 10 ---
    #[test]
    fn out_of_order_fixture_gives_a_deterministic_lowest_numbered_pick() {
        // Two nodes on the same interface number, inserted highest-first. The
        // documented rule is "ascending hidrawN order, first match wins", so
        // hidraw2 must win every time regardless of insertion order.
        let sysfs = MockSysfs::new()
            .with_node("hidraw9", info(BLACKWIDOW_V4_PRO, 3, PROTO_CONTROL))
            .with_node("hidraw2", info(BLACKWIDOW_V4_PRO, 3, PROTO_CONTROL))
            .with_node("hidraw11", info(BLACKWIDOW_V4_PRO, 3, PROTO_CONTROL));

        for i in 0..100 {
            let node = find_device(&sysfs, RAZER, BLACKWIDOW_V4_PRO, 3).expect("a match exists");
            assert_eq!(node.name, "hidraw2", "non-deterministic on iteration {i}");
        }
    }

    #[test]
    fn numeric_not_lexicographic_ordering() {
        // "hidraw11" sorts before "hidraw2" lexicographically; it must not here.
        let mut names = vec!["hidraw11".to_owned(), "hidraw2".to_owned()];
        sort_by_index(&mut names);
        assert_eq!(names, vec!["hidraw2".to_owned(), "hidraw11".to_owned()]);
    }

    #[test]
    fn unparseable_names_sort_last_and_do_not_panic() {
        let mut names = vec!["weird".to_owned(), "hidraw5".to_owned()];
        sort_by_index(&mut names);
        assert_eq!(names, vec!["hidraw5".to_owned(), "weird".to_owned()]);
    }

    // --- criterion 11 ---
    #[test]
    fn nodes_with_no_usb_parent_are_skipped_not_fatal() {
        // hidraw0 is a bluetooth node: interface_info yields Ok(None).
        let sysfs = MockSysfs::new()
            .without_usb_parent("hidraw0")
            .with_node("hidraw1", info(BLACKWIDOW_V4_PRO, 3, PROTO_CONTROL));
        let node = find_device(&sysfs, RAZER, BLACKWIDOW_V4_PRO, 3)
            .expect("the bluetooth node must be skipped, not error");
        assert_eq!(node.name, "hidraw1");
    }

    // --- criterion 12: the cross-crate assertion ---
    #[test]
    fn report_index_selects_the_control_interface_for_every_table_entry() {
        for entry in razer_devices::all() {
            let sysfs = fixture_for(entry.pid);
            let node = find_device(
                &sysfs,
                razer_devices::VENDOR_ID,
                entry.pid,
                entry.report_index,
            )
            .unwrap_or_else(|e| {
                panic!("{} (pid {:#06x}): {e}", entry.name, entry.pid);
            });
            assert_eq!(
                node.info.interface_protocol, PROTO_CONTROL,
                "{}: report_index {} did not land on the control interface",
                entry.name, entry.report_index
            );
            assert_eq!(node.info.interface_number, entry.report_index);
        }
    }

    #[test]
    fn list_razer_nodes_returns_every_node_in_ascending_order() {
        let nodes =
            list_razer_nodes(&MockSysfs::blackwidow_v4_pro()).expect("the fixture enumerates");
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["hidraw0", "hidraw1", "hidraw2", "hidraw3"]);
    }

    #[test]
    fn list_razer_nodes_omits_non_razer_and_parentless_nodes() {
        let sysfs = MockSysfs::new()
            .without_usb_parent("hidraw0")
            .with_node(
                "hidraw1",
                UsbInterfaceInfo {
                    vendor_id: 0x046D, // Logitech
                    product_id: 0xC52B,
                    interface_number: 0,
                    interface_protocol: 0x01,
                },
            )
            .with_node("hidraw2", info(BLACKWIDOW_V4_PRO, 3, PROTO_CONTROL));

        let nodes = list_razer_nodes(&sysfs).expect("the fixture enumerates");
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["hidraw2"]);
    }
}
