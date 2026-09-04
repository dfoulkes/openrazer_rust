// SPDX-License-Identifier: GPL-2.0-or-later
//! In-memory fixtures. Every test in this crate runs against these.
//!
//! Nothing here performs a syscall, opens a file, or sleeps. That is the point:
//! `cargo test -p razer-hid` must be safe to run on the machine whose only
//! keyboard is the device under study.

use core::time::Duration;
use std::collections::VecDeque;
use std::path::PathBuf;

use razer_proto::RazerReport;

use crate::error::HidError;
use crate::session::Clock;
use crate::sysfs::{SysfsSource, UsbInterfaceInfo};
use crate::transport::{FeatureTransport, HIDRAW_BUF_LEN};

/// `USB_INTERFACE_PROTOCOL_MOUSE`. On both devices in scope this marks the
/// control interface — the one the C driver's report index points at.
const PROTO_CONTROL: u8 = 0x02;
/// `USB_INTERFACE_PROTOCOL_KEYBOARD`.
const PROTO_KEYBOARD: u8 = 0x01;
/// `USB_INTERFACE_PROTOCOL_NONE`.
const PROTO_NONE: u8 = 0x00;

/// `USB_VENDOR_ID_RAZER`, `razercommon.h:26`.
const RAZER: u16 = 0x1532;

// ---------------------------------------------------------------------------
// MockSysfs
// ---------------------------------------------------------------------------

/// A fake `/sys/class/hidraw` tree.
///
/// Nodes are returned by [`SysfsSource::hidraw_names`] in insertion order;
/// [`crate::find_device`] sorts them itself, so a deliberately shuffled fixture
/// is the way to prove the ordering rule.
#[derive(Debug, Clone, Default)]
pub struct MockSysfs {
    /// `(name, Some(info))` for a USB node, `(name, None)` for one with no USB
    /// ancestor.
    nodes: Vec<(String, Option<UsbInterfaceInfo>)>,
}

impl MockSysfs {
    /// An empty tree.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add one fake hidraw node with a USB parent.
    #[must_use]
    pub fn with_node(mut self, name: &str, info: UsbInterfaceInfo) -> Self {
        self.nodes.push((name.to_owned(), Some(info)));
        self
    }

    /// Add a node whose `interface_info` yields `Ok(None)` — a Bluetooth, I2C
    /// or virtual HID device.
    ///
    /// Not in the original frozen sketch, but there is no other way to exercise
    /// the "skip, do not fail" rule that enumeration depends on.
    #[must_use]
    pub fn without_usb_parent(mut self, name: &str) -> Self {
        self.nodes.push((name.to_owned(), None));
        self
    }

    /// The BlackWidow V4 Pro's real interface layout.
    ///
    /// Four hidraw nodes; interface 3 (`hidraw3`, protocol `0x02`) is the
    /// control interface the device table's `report_index = 0x03` points at.
    #[must_use]
    pub fn blackwidow_v4_pro() -> Self {
        const PID: u16 = 0x028D;
        Self::new()
            .with_node("hidraw0", usb(PID, 0, PROTO_KEYBOARD))
            .with_node("hidraw1", usb(PID, 1, PROTO_NONE))
            .with_node("hidraw2", usb(PID, 2, PROTO_NONE))
            .with_node("hidraw3", usb(PID, 3, PROTO_CONTROL))
    }

    /// The Basilisk V3 Pro (wireless) interface layout.
    ///
    /// Three hidraw nodes; interface 0 (`hidraw4`, protocol `0x02`) is the
    /// control interface, matching `report_index = 0x00`. The node numbers
    /// start at 4 deliberately — a mouse plugged in after a keyboard does not
    /// get `hidraw0`, and code that assumes it does is wrong.
    #[must_use]
    pub fn basilisk_v3_pro_wireless() -> Self {
        const PID: u16 = 0x00AB;
        Self::new()
            .with_node("hidraw4", usb(PID, 0, PROTO_CONTROL))
            .with_node("hidraw5", usb(PID, 1, PROTO_KEYBOARD))
            .with_node("hidraw6", usb(PID, 2, PROTO_NONE))
    }
}

/// Shorthand for a Razer-vendor interface descriptor.
fn usb(pid: u16, interface_number: u8, interface_protocol: u8) -> UsbInterfaceInfo {
    UsbInterfaceInfo {
        vendor_id: RAZER,
        product_id: pid,
        interface_number,
        interface_protocol,
    }
}

impl SysfsSource for MockSysfs {
    fn hidraw_names(&self) -> Result<Vec<String>, HidError> {
        Ok(self.nodes.iter().map(|(n, _)| n.clone()).collect())
    }

    fn interface_info(&self, hidraw_name: &str) -> Result<Option<UsbInterfaceInfo>, HidError> {
        Ok(self
            .nodes
            .iter()
            .find(|(n, _)| n == hidraw_name)
            .and_then(|(_, info)| info.clone()))
    }

    fn device_path(&self, hidraw_name: &str) -> PathBuf {
        PathBuf::from("/dev").join(hidraw_name)
    }
}

// ---------------------------------------------------------------------------
// MockTransport
// ---------------------------------------------------------------------------

/// A fake device: records every SET, replays queued GETs.
///
/// Zero syscalls, zero I/O.
#[derive(Debug, Clone, Default)]
pub struct MockTransport {
    sent: Vec<[u8; HIDRAW_BUF_LEN]>,
    responses: VecDeque<[u8; HIDRAW_BUF_LEN]>,
    fail_next_set: Option<i32>,
}

impl MockTransport {
    /// A transport with nothing sent and nothing queued.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue one report to be handed back by the next `get_feature`.
    ///
    /// The report is encoded exactly as a device would encode it, checksum
    /// included — `RazerReport::to_bytes` recomputes byte 88.
    pub fn push_response(&mut self, report: &RazerReport) {
        let mut buf = [0u8; HIDRAW_BUF_LEN];
        buf[1..].copy_from_slice(&report.to_bytes());
        self.responses.push_back(buf);
    }

    /// Queue a raw 91-byte buffer, for malformed-response tests.
    pub fn push_raw(&mut self, buf: [u8; HIDRAW_BUF_LEN]) {
        self.responses.push_back(buf);
    }

    /// Every buffer passed to `set_feature`, in order.
    #[must_use]
    pub fn sent(&self) -> &[[u8; HIDRAW_BUF_LEN]] {
        &self.sent
    }

    /// [`MockTransport::sent`] decoded back into reports.
    #[must_use]
    pub fn sent_reports(&self) -> Vec<RazerReport> {
        self.sent
            .iter()
            .map(|buf| {
                let mut report = [0u8; 90];
                report.copy_from_slice(&buf[1..]);
                RazerReport::from_bytes(&report)
            })
            .collect()
    }

    /// Make the next `set_feature` fail with this errno, then behave normally.
    pub fn fail_next_set(&mut self, errno: i32) {
        self.fail_next_set = Some(errno);
    }

    /// How many queued responses remain unconsumed.
    #[must_use]
    pub fn pending_responses(&self) -> usize {
        self.responses.len()
    }
}

impl FeatureTransport for MockTransport {
    fn set_feature(&mut self, buf: &[u8; HIDRAW_BUF_LEN]) -> Result<(), HidError> {
        if let Some(errno) = self.fail_next_set.take() {
            return Err(HidError::Io {
                op: "HIDIOCSFEATURE",
                errno,
            });
        }
        self.sent.push(*buf);
        Ok(())
    }

    /// Pop the next queued response.
    ///
    /// With the queue empty there is nothing to read, so this reports zero
    /// bytes — which [`crate::Session`] turns into [`HidError::ShortRead`],
    /// exactly as a real device that answered with a truncated feature report
    /// would.
    fn get_feature(&mut self, buf: &mut [u8; HIDRAW_BUF_LEN]) -> Result<usize, HidError> {
        match self.responses.pop_front() {
            Some(response) => {
                *buf = response;
                Ok(HIDRAW_BUF_LEN)
            }
            None => Ok(0),
        }
    }
}

// ---------------------------------------------------------------------------
// MockClock
// ---------------------------------------------------------------------------

/// Records sleeps instead of performing them.
///
/// The per-device wait matters — 600 µs for the keyboard, 31 ms for the mouse —
/// and "did we actually wait?" is only assertable if the wait is observable.
#[derive(Debug, Clone, Default)]
pub struct MockClock {
    sleeps: Vec<Duration>,
}

impl MockClock {
    /// A clock that has slept for nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Every sleep requested, in order.
    #[must_use]
    pub fn sleeps(&self) -> &[Duration] {
        &self.sleeps
    }

    /// The sum of every sleep requested.
    #[must_use]
    pub fn total(&self) -> Duration {
        self.sleeps.iter().sum()
    }
}

impl Clock for MockClock {
    fn sleep(&mut self, d: Duration) {
        self.sleeps.push(d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blackwidow_fixture_matches_the_documented_layout() {
        let sysfs = MockSysfs::blackwidow_v4_pro();
        assert_eq!(
            sysfs.hidraw_names().expect("names"),
            ["hidraw0", "hidraw1", "hidraw2", "hidraw3"]
        );
        let control = sysfs
            .interface_info("hidraw3")
            .expect("lookup")
            .expect("hidraw3 has a usb parent");
        assert_eq!(control.vendor_id, RAZER);
        assert_eq!(control.product_id, 0x028D);
        assert_eq!(control.interface_number, 3);
        assert_eq!(control.interface_protocol, PROTO_CONTROL);
    }

    #[test]
    fn basilisk_fixture_matches_the_documented_layout() {
        let sysfs = MockSysfs::basilisk_v3_pro_wireless();
        assert_eq!(
            sysfs.hidraw_names().expect("names"),
            ["hidraw4", "hidraw5", "hidraw6"]
        );
        let control = sysfs
            .interface_info("hidraw4")
            .expect("lookup")
            .expect("hidraw4 has a usb parent");
        assert_eq!(control.product_id, 0x00AB);
        assert_eq!(control.interface_number, 0);
        assert_eq!(control.interface_protocol, PROTO_CONTROL);
    }

    #[test]
    fn unknown_node_has_no_interface_info() {
        let sysfs = MockSysfs::blackwidow_v4_pro();
        assert_eq!(sysfs.interface_info("hidraw99").expect("lookup"), None);
    }

    #[test]
    fn mock_sysfs_preserves_insertion_order() {
        let sysfs = MockSysfs::new()
            .with_node("hidraw9", usb(0x028D, 3, PROTO_CONTROL))
            .with_node("hidraw2", usb(0x028D, 0, PROTO_NONE));
        assert_eq!(sysfs.hidraw_names().expect("names"), ["hidraw9", "hidraw2"]);
    }

    #[test]
    fn transport_records_sets_and_replays_gets() {
        let mut t = MockTransport::new();
        assert!(t.sent().is_empty());

        let mut out = [0u8; HIDRAW_BUF_LEN];
        out[1] = 0xAB;
        t.set_feature(&out).expect("set");
        assert_eq!(t.sent().len(), 1);
        assert_eq!(t.sent()[0][1], 0xAB);

        let response = RazerReport::new(0x00, 0x84, 0x02);
        t.push_response(&response);
        let mut buf = [0u8; HIDRAW_BUF_LEN];
        assert_eq!(t.get_feature(&mut buf).expect("get"), HIDRAW_BUF_LEN);
        assert_eq!(buf[0], 0x00, "report id prefix");
        // The report starts at buf[1], so its byte 7 (command_id) is buf[8].
        assert_eq!(buf[8], 0x84, "command id lands at report byte 7");
    }

    #[test]
    fn transport_with_an_empty_queue_reports_zero_bytes() {
        let mut t = MockTransport::new();
        let mut buf = [0u8; HIDRAW_BUF_LEN];
        assert_eq!(t.get_feature(&mut buf).expect("get"), 0);
    }

    #[test]
    fn fail_next_set_fires_once_only() {
        let mut t = MockTransport::new();
        t.fail_next_set(libc::EACCES);
        let buf = [0u8; HIDRAW_BUF_LEN];
        assert!(matches!(
            t.set_feature(&buf),
            Err(HidError::Io {
                op: "HIDIOCSFEATURE",
                errno: 13
            })
        ));
        assert!(t.sent().is_empty(), "a failed set must not be recorded");
        t.set_feature(&buf).expect("the second set succeeds");
        assert_eq!(t.sent().len(), 1);
    }

    #[test]
    fn sent_reports_round_trips_what_was_sent() {
        let mut t = MockTransport::new();
        let report = RazerReport::new(0x0F, 0x04, 0x03)
            .with_args(&[0x01, 0x05, 0x40])
            .with_transaction_id(0x1F);
        let mut buf = [0u8; HIDRAW_BUF_LEN];
        buf[1..].copy_from_slice(&report.to_bytes());
        t.set_feature(&buf).expect("set");

        let decoded = t.sent_reports();
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].command_class, 0x0F);
        assert_eq!(decoded[0].command_id, 0x04);
        assert_eq!(decoded[0].transaction_id, 0x1F);
        assert_eq!(decoded[0].args(), &[0x01, 0x05, 0x40]);
    }

    #[test]
    fn clock_records_instead_of_sleeping() {
        let mut c = MockClock::new();
        assert_eq!(c.total(), Duration::ZERO);
        c.sleep(Duration::from_micros(600));
        c.sleep(Duration::from_millis(10));
        assert_eq!(
            c.sleeps(),
            [Duration::from_micros(600), Duration::from_millis(10)]
        );
        assert_eq!(c.total(), Duration::from_micros(10_600));
    }
}
