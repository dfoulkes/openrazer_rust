// SPDX-License-Identifier: GPL-2.0-or-later
//! The static device table for the two devices in scope.
//!
//! Every field is transcribed from `the upstream OpenRazer `driver/` tree` and cited
//! at the point of use. Do not add a fourth device: the Razer Kiyo Pro (0x0E05)
//! is a webcam with no Chroma / `razer_report` support and is deliberately absent.

use core::time::Duration;

use crate::capabilities::Capabilities;

/// `USB_VENDOR_ID_RAZER`, razercommon.h:26.
pub const VENDOR_ID: u16 = 0x1532;

/// Which physical class of device an entry describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Keyboard,
    Mouse,
}

/// Which of the two polling-rate command families this device speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollRateKind {
    /// `razer_chroma_misc_set/get_polling_rate` (0x00/0x05, 0x00/0x85) —
    /// razerchromacommon.c:1092-1136. Encodes only 1000/500/125 Hz.
    Legacy,
    /// `razer_chroma_misc_set/get_polling_rate2` (0x00/0x40, 0x00/0xC0) —
    /// razerchromacommon.c:1138-1189.
    V2,
}

/// A single device's protocol parameters and supported commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceEntry {
    /// USB product id.
    pub pid: u16,
    /// Human-readable model name, for logs and CLI output.
    pub name: &'static str,
    pub kind: DeviceKind,
    /// The ONE transaction id used for every command to this device.
    /// Byte 1 of the report. This is the value the C driver gets wrong
    /// in `razer_attr_write_device_mode()` (razerkbd_driver.c:4308, hardcodes
    /// 0xFF instead of the 0x1F used everywhere else for this device).
    pub transaction_id: u8,
    /// The C driver's "report index" == the USB control transfer wIndex ==
    /// the bInterfaceNumber of the control interface. Used by razer-hid to
    /// pick the correct /dev/hidraw* node.
    pub report_index: u8,
    /// The same value as report_index in every device in scope; kept separate
    /// because the C API distinguishes them (razer_get_usb_response).
    pub response_index: u8,
    /// Mandatory delay after every SET_REPORT (`fsleep(wait)` in
    /// `razer_send_control_msg`).
    pub wait: Duration,
    pub capabilities: Capabilities,
    /// LedId byte to use for brightness and matrix-effect commands on this
    /// device. 0x05 BACKLIGHT_LED for the keyboard, 0x00 ZERO_LED for the mouse.
    /// Constants: razercommon.h:37 (ZERO_LED), razercommon.h:41 (BACKLIGHT_LED).
    pub default_led: u8,
    pub poll_rate_kind: PollRateKind,
}

/// Capabilities shared by every device in scope: device mode, firmware
/// version, serial, brightness, static effect and poll rate are all
/// supported by both the keyboard and mouse code paths in the C driver.
const COMMON_CAPS: Capabilities = Capabilities(
    Capabilities::DEVICE_MODE.bits()
        | Capabilities::FIRMWARE_VERSION.bits()
        | Capabilities::SERIAL.bits()
        | Capabilities::BRIGHTNESS.bits()
        | Capabilities::STATIC_EFFECT.bits()
        | Capabilities::POLL_RATE.bits(),
);

/// `ZERO_LED`, razercommon.h:37.
const ZERO_LED: u8 = 0x00;
/// `BACKLIGHT_LED`, razercommon.h:41.
const BACKLIGHT_LED: u8 = 0x05;

/// The complete table. Exactly three entries, sorted ascending by pid.
const TABLE: [DeviceEntry; 3] = [
    // Razer Basilisk V3 Pro (Wired) — PID 0x00AA.
    // razermouse_driver.h:92 USB_DEVICE_ID_RAZER_BASILISK_V3_PRO_WIRED.
    DeviceEntry {
        pid: 0x00AA,
        name: "Razer Basilisk V3 Pro (Wired)",
        kind: DeviceKind::Mouse,
        // transaction id 0x1f, e.g. razer_attr_write_device_mode,
        // razermouse_driver.c:3900-3901.
        transaction_id: 0x1F,
        // razer_get_report(), razermouse_driver.c:36 `unsigned int index = 0;`
        // — the Basilisk V3 Pro pair fall into the first case block (lines
        // 60-61) which does not reassign `index`.
        report_index: 0x00,
        response_index: 0x00,
        // RAZER_NEW_MOUSE_RECEIVER_WAIT_US, razermouse_driver.h:131.
        wait: Duration::from_micros(31_000),
        capabilities: COMMON_CAPS,
        default_led: ZERO_LED,
        // razer_attr_write_poll_rate, razermouse_driver.c:2117-2140 — uses
        // razer_chroma_misc_set_polling_rate, the legacy family.
        poll_rate_kind: PollRateKind::Legacy,
    },
    // Razer Basilisk V3 Pro (Wireless) — PID 0x00AB.
    // razermouse_driver.h:93 USB_DEVICE_ID_RAZER_BASILISK_V3_PRO_WIRELESS.
    DeviceEntry {
        pid: 0x00AB,
        name: "Razer Basilisk V3 Pro (Wireless)",
        kind: DeviceKind::Mouse,
        transaction_id: 0x1F,
        report_index: 0x00,
        response_index: 0x00,
        wait: Duration::from_micros(31_000),
        capabilities: COMMON_CAPS,
        default_led: ZERO_LED,
        poll_rate_kind: PollRateKind::Legacy,
    },
    // Razer BlackWidow V4 Pro — PID 0x028D.
    // razerkbd_driver.h:99 USB_DEVICE_ID_RAZER_BLACKWIDOW_V4_PRO.
    DeviceEntry {
        pid: 0x028D,
        name: "Razer BlackWidow V4 Pro",
        kind: DeviceKind::Keyboard,
        // razer_set_device_mode()'s per-PID switch, razerkbd_driver.c:536
        // (case label) falling into `transaction_id.id = 0x1F;` at line 546.
        // NOT the 0xFF hardcoded by the buggy razer_attr_write_device_mode()
        // at razerkbd_driver.c:4308 — that bug is why this project exists.
        transaction_id: 0x1F,
        // razer_get_report_params(), razerkbd_driver.c: V4 Pro case sets
        // `*report_index = 0x03;`.
        report_index: 0x03,
        // same block: `*response_index = 0x03;`.
        response_index: 0x03,
        // RAZER_BLACKWIDOW_CHROMA_WAIT_US, razerkbd_driver.h:163.
        wait: Duration::from_micros(600),
        capabilities: COMMON_CAPS,
        default_led: BACKLIGHT_LED,
        // razer_attr_write_poll_rate / razer_attr_read_poll_rate, V4 Pro
        // block — razer_chroma_misc_set/get_polling_rate2, the v2 family.
        poll_rate_kind: PollRateKind::V2,
    },
];

/// The complete table. Exactly three entries. Sorted ascending by pid.
pub fn all() -> &'static [DeviceEntry] {
    &TABLE
}

/// O(n) linear scan of [`all`]. Returns `None` for any pid not in the table —
/// including the Kiyo Pro 0x0E05, which is deliberately absent.
pub fn lookup(pid: u16) -> Option<&'static DeviceEntry> {
    TABLE.iter().find(|e| e.pid == pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_exactly_three_entries() {
        assert_eq!(all().len(), 3);
    }

    #[test]
    fn table_sorted_ascending_by_pid() {
        for w in all().windows(2) {
            assert!(
                w[0].pid < w[1].pid,
                "table not sorted: {:#x} >= {:#x}",
                w[0].pid,
                w[1].pid
            );
        }
    }

    #[test]
    fn pids_are_exactly_the_expected_set() {
        let mut pids: Vec<u16> = all().iter().map(|e| e.pid).collect();
        pids.sort_unstable();
        assert_eq!(pids, vec![0x00AA, 0x00AB, 0x028D]);
    }

    #[test]
    fn vendor_id_is_razer() {
        assert_eq!(VENDOR_ID, 0x1532);
    }

    #[test]
    fn blackwidow_v4_pro_fields() {
        let e = lookup(0x028D).expect("BlackWidow V4 Pro must be in the table");
        assert!(e.name.contains("BlackWidow V4 Pro"));
        assert_eq!(e.kind, DeviceKind::Keyboard);
        assert_eq!(e.transaction_id, 0x1F);
        assert_eq!(e.report_index, 3);
        assert_eq!(e.response_index, 3);
        assert_eq!(e.wait, Duration::from_micros(600));
        assert_eq!(e.default_led, 0x05);
        assert_eq!(e.poll_rate_kind, PollRateKind::V2);
    }

    #[test]
    fn basilisk_v3_pro_wired_fields() {
        let e = lookup(0x00AA).expect("Basilisk V3 Pro Wired must be in the table");
        assert!(e.name.contains("Basilisk V3 Pro"));
        assert!(e.name.contains("Wired"));
        assert_eq!(e.kind, DeviceKind::Mouse);
        assert_eq!(e.transaction_id, 0x1F);
        assert_eq!(e.report_index, 0);
        assert_eq!(e.response_index, 0);
        assert_eq!(e.wait, Duration::from_micros(31_000));
        assert_eq!(e.default_led, 0x00);
        assert_eq!(e.poll_rate_kind, PollRateKind::Legacy);
    }

    #[test]
    fn basilisk_v3_pro_wireless_fields() {
        let e = lookup(0x00AB).expect("Basilisk V3 Pro Wireless must be in the table");
        assert!(e.name.contains("Basilisk V3 Pro"));
        assert!(e.name.contains("Wireless"));
        assert_eq!(e.kind, DeviceKind::Mouse);
        assert_eq!(e.transaction_id, 0x1F);
        assert_eq!(e.report_index, 0);
        assert_eq!(e.response_index, 0);
        assert_eq!(e.wait, Duration::from_micros(31_000));
        assert_eq!(e.default_led, 0x00);
        assert_eq!(e.poll_rate_kind, PollRateKind::Legacy);
    }

    #[test]
    fn all_entries_have_common_capabilities() {
        for e in all() {
            assert!(
                e.capabilities.contains(COMMON_CAPS),
                "{} missing common capabilities",
                e.name
            );
        }
    }

    #[test]
    fn kiyo_pro_is_absent() {
        assert!(lookup(0x0E05).is_none());
    }

    #[test]
    fn unknown_pids_return_none() {
        assert!(lookup(0x0000).is_none());
        assert!(lookup(0xFFFF).is_none());
        assert!(lookup(0x028C).is_none());
        assert!(lookup(0x028E).is_none());
    }

    #[test]
    fn no_entry_uses_the_buggy_0xff_transaction_id() {
        // Regression guard for the C driver bug this project exists to fix.
        for e in all() {
            assert_ne!(
                e.transaction_id, 0xFF,
                "{} must not use the buggy 0xFF transaction id",
                e.name
            );
        }
    }

    #[test]
    fn report_index_always_equals_response_index() {
        for e in all() {
            assert_eq!(
                e.report_index, e.response_index,
                "{} report/response index mismatch",
                e.name
            );
        }
    }
}
