// SPDX-License-Identifier: GPL-2.0-or-later
//! Adversarial integration tests for `razer-hid`.
//!
//! These are written from the C driver rather than from the implementation, and
//! deliberately duplicate constants (5 attempts, 10 ms, 0x1F, 600 us, 31 ms,
//! 0xC0/0x85/0x40/0x05) as literals so that a change to a `const` in the crate
//! cannot silently move the goalposts. The checksum is recomputed here with its
//! own XOR loop rather than by calling `razer_proto::crc`, so a wrong CRC
//! implementation cannot validate itself.
//!
//! Everything runs against in-memory fixtures. No syscall, no device node, no
//! sysfs read, no real sleep. `HardwareOptIn` is never constructed.

use core::time::Duration;

use razer_devices::capabilities::Capabilities;
use razer_devices::table::{DeviceEntry, DeviceKind, PollRateKind};
use razer_hid::sysfs::{SysfsSource, UsbInterfaceInfo};
use razer_hid::{
    Clock, FeatureTransport, HIDRAW_BUF_LEN, HidError, MockClock, MockSysfs, MockTransport,
    Session, find_device, list_razer_nodes,
};
use razer_proto::report::{LedId, Status, Storage};
use razer_proto::{DeviceMode, PollRate, RazerReport, Rgb, cmd};

const RAZER: u16 = 0x1532;
const BLACKWIDOW_V4_PRO: u16 = 0x028D;
const BASILISK_WIRELESS: u16 = 0x00AB;
const BASILISK_WIRED: u16 = 0x00AA;

/// `razerkbd_driver.h:163` `RAZER_BLACKWIDOW_CHROMA_WAIT_US 600`.
const KBD_WAIT: Duration = Duration::from_micros(600);
/// `razermouse_driver.h:131` `RAZER_NEW_MOUSE_RECEIVER_WAIT_US 31000`.
const MOUSE_WAIT: Duration = Duration::from_micros(31_000);
/// `razer_send_payload()`: `fsleep(10000)`.
const RETRY_DELAY: Duration = Duration::from_millis(10);
/// `razer_send_payload()`: `for (retry = 5; retry > 0; retry--)`.
const MAX_ATTEMPTS: usize = 5;

fn entry(pid: u16) -> &'static DeviceEntry {
    razer_devices::lookup(pid).unwrap_or_else(|| panic!("{pid:#06x} must be in the table"))
}

/// The wait each device is required to observe after every SET, straight from
/// the C driver's per-PID tables — not read back from `DeviceEntry`.
fn required_wait(pid: u16) -> Duration {
    match pid {
        BLACKWIDOW_V4_PRO => KBD_WAIT,
        BASILISK_WIRED | BASILISK_WIRELESS => MOUSE_WAIT,
        other => panic!("no wait known for {other:#06x}"),
    }
}

/// Independent reimplementation of `razer_calculate_crc()`, `razercommon.c:111`:
/// `for (i = 2; i < 88; i++) crc ^= _report[i];`
///
/// Takes the 90-byte report (NOT the 91-byte hidraw buffer).
fn c_crc(report: &[u8]) -> u8 {
    assert_eq!(report.len(), 90);
    let mut crc = 0u8;
    let mut i = 2;
    while i < 88 {
        crc ^= report[i];
        i += 1;
    }
    crc
}

/// Hand-build the 91-byte hidraw buffer the C driver would put on the wire,
/// from first principles: report-id prefix, then the 90-byte `struct
/// razer_report` laid out per `razercommon.h:124-136`.
fn golden(tid: u8, data_size: u8, command_class: u8, command_id: u8, args: &[u8]) -> [u8; 91] {
    let mut report = [0u8; 90];
    report[0] = 0x00; // status: host always sends 0x00
    report[1] = tid;
    report[2] = 0x00; // remaining_packets, big endian
    report[3] = 0x00;
    report[4] = 0x00; // protocol_type
    report[5] = data_size;
    report[6] = command_class;
    report[7] = command_id;
    report[8..8 + args.len()].copy_from_slice(args);
    report[88] = c_crc(&report);
    report[89] = 0x00; // reserved

    let mut buf = [0u8; 91];
    buf[1..].copy_from_slice(&report);
    buf
}

/// Every frame that ever left a session must be well-formed, checked against
/// the independent CRC above.
fn assert_all_frames_valid(sent: &[[u8; HIDRAW_BUF_LEN]]) {
    for (i, buf) in sent.iter().enumerate() {
        assert_eq!(buf[0], 0x00, "frame {i}: hidraw report id must be 0");
        assert_eq!(buf[1], 0x00, "frame {i}: host status byte must be 0x00");
        assert_eq!(buf[5], 0x00, "frame {i}: protocol_type must be 0x00");
        assert!(
            buf[6] <= 80,
            "frame {i}: data_size {} exceeds the 80-byte argument array",
            buf[6]
        );
        assert_eq!(buf[89], c_crc(&buf[1..]), "frame {i}: checksum");
        assert_eq!(buf[90], 0x00, "frame {i}: reserved byte must be 0");
    }
}

// ---------------------------------------------------------------------------
// Test doubles the crate's own mocks cannot express
// ---------------------------------------------------------------------------

/// A device that answers every SET with a well-formed success echo.
///
/// Lets a whole typed operation be driven end to end without hand-queueing a
/// response per attempt, which is what makes the "every command on every
/// device" sweeps below possible.
struct EchoTransport {
    sent: Vec<[u8; HIDRAW_BUF_LEN]>,
    reply_args: Vec<u8>,
    pending: Option<[u8; HIDRAW_BUF_LEN]>,
}

impl EchoTransport {
    fn new(reply_args: &[u8]) -> Self {
        Self {
            sent: Vec::new(),
            reply_args: reply_args.to_vec(),
            pending: None,
        }
    }
}

impl FeatureTransport for EchoTransport {
    fn set_feature(&mut self, buf: &[u8; HIDRAW_BUF_LEN]) -> Result<(), HidError> {
        self.sent.push(*buf);

        let mut report = [0u8; 90];
        report.copy_from_slice(&buf[1..]);
        let request = RazerReport::from_bytes(&report);

        let mut response =
            RazerReport::new(request.command_class, request.command_id, request.data_size)
                .with_args(&self.reply_args)
                .with_transaction_id(request.transaction_id);
        response.status = Status::Successful;
        response.remaining_packets = request.remaining_packets;

        let mut out = [0u8; HIDRAW_BUF_LEN];
        out[1..].copy_from_slice(&response.to_bytes());
        self.pending = Some(out);
        Ok(())
    }

    fn get_feature(&mut self, buf: &mut [u8; HIDRAW_BUF_LEN]) -> Result<usize, HidError> {
        match self.pending.take() {
            Some(r) => {
                *buf = r;
                Ok(HIDRAW_BUF_LEN)
            }
            None => Ok(0),
        }
    }
}

/// A device whose `HIDIOCGFEATURE` returns a specific byte count, or an errno.
struct FaultyGetTransport {
    sent: usize,
    outcome: Result<usize, i32>,
}

impl FeatureTransport for FaultyGetTransport {
    fn set_feature(&mut self, _buf: &[u8; HIDRAW_BUF_LEN]) -> Result<(), HidError> {
        self.sent += 1;
        Ok(())
    }

    fn get_feature(&mut self, _buf: &mut [u8; HIDRAW_BUF_LEN]) -> Result<usize, HidError> {
        match self.outcome {
            Ok(n) => Ok(n),
            Err(errno) => Err(HidError::Io {
                op: "HIDIOCGFEATURE",
                errno,
            }),
        }
    }
}

/// A `/sys` that cannot be read.
struct BrokenSysfs {
    fail_listing: bool,
}

impl SysfsSource for BrokenSysfs {
    fn hidraw_names(&self) -> Result<Vec<String>, HidError> {
        if self.fail_listing {
            Err(HidError::Io {
                op: "read_dir(/sys/class/hidraw)",
                errno: libc::EACCES,
            })
        } else {
            Ok(vec!["hidraw0".to_owned()])
        }
    }

    fn interface_info(&self, _name: &str) -> Result<Option<UsbInterfaceInfo>, HidError> {
        Err(HidError::Sysfs("idProduct: \"zz\" is not hex".into()))
    }

    fn device_path(&self, name: &str) -> std::path::PathBuf {
        std::path::PathBuf::from("/dev").join(name)
    }
}

fn info(vid: u16, pid: u16, iface: u8, proto: u8) -> UsbInterfaceInfo {
    UsbInterfaceInfo {
        vendor_id: vid,
        product_id: pid,
        interface_number: iface,
        interface_protocol: proto,
    }
}

// ===========================================================================
// 1. The headline: byte 1, on every command, on every device
// ===========================================================================

/// The whole reason this project exists, generalised past `set_device_mode`.
///
/// `razerkbd_driver.c:4308` hardcodes `request.transaction_id.id = 0xFF;` while
/// the driver's own per-PID switch at `:536`/`:546` says `0x1F` for the
/// BlackWidow V4 Pro. Here EVERY typed operation on EVERY device in the table
/// is driven and every single frame's byte 1 is checked.
#[test]
fn no_command_on_any_device_ever_emits_a_transaction_id_other_than_the_tables() {
    for entry in razer_devices::all() {
        let expected = entry.transaction_id;
        assert_eq!(
            expected, 0x1F,
            "{}: the C driver uses 0x1f for this pid",
            entry.name
        );

        // Reply payload wide enough to satisfy every parser (serial reads 22).
        let mut reply = vec![0u8; 24];
        reply[0] = 0x01; // legacy poll-rate code / firmware major / mode
        reply[1] = 0x08; // v2 poll-rate code (1000 Hz)
        reply[2] = 0x40; // brightness
        let mut transport = EchoTransport::new(&reply);
        let mut clock = MockClock::new();
        {
            let mut s = Session::new(entry, &mut transport, &mut clock);
            s.set_device_mode(DeviceMode::Driver).expect("device mode");
            s.get_device_mode().expect("get device mode");
            s.firmware_version().expect("firmware");
            s.serial().expect("serial");
            s.set_brightness(0x40).expect("set brightness");
            s.get_brightness().expect("get brightness");
            s.set_static_effect(Rgb::new(0x11, 0x22, 0x33))
                .expect("static effect");
            s.set_poll_rate(PollRate::Hz1000).expect("set poll rate");
            s.get_poll_rate().expect("get poll rate");
        }

        assert_eq!(
            transport.sent.len(),
            9,
            "{}: every operation must reach the wire exactly once",
            entry.name
        );
        for (i, buf) in transport.sent.iter().enumerate() {
            assert_eq!(
                buf[2], expected,
                "{}: frame {i} (class {:#04x} id {:#04x}) carries transaction id {:#04x}, not {expected:#04x}",
                entry.name, buf[7], buf[8], buf[2]
            );
            assert_ne!(
                buf[2], 0xFF,
                "{}: frame {i} reintroduced razerkbd_driver.c:4308's 0xFF",
                entry.name
            );
            assert_ne!(
                buf[2], 0x00,
                "{}: frame {i} left byte 1 unstamped — the C driver WARN_ONs on this \
                 (razercommon.c:79, razerkbd_driver.c:412)",
                entry.name
            );
        }
        assert_all_frames_valid(&transport.sent);
    }
}

/// Two independently hand-built golden frames for the command that carries the
/// bug, byte for byte, for both modes.
#[test]
fn set_device_mode_frames_match_a_hand_built_golden_buffer() {
    for (mode, mode_byte) in [(DeviceMode::Normal, 0x00u8), (DeviceMode::Driver, 0x03)] {
        let mut transport = MockTransport::new();
        let mut clock = MockClock::new();
        {
            let mut s = Session::new(entry(BLACKWIDOW_V4_PRO), &mut transport, &mut clock);
            s.send(&cmd::set_device_mode(mode)).expect("send");
        }
        // razer_chroma_standard_set_device_mode: get_razer_report(0x00, 0x04, 0x02),
        // arguments[0] = mode, arguments[1] = 0x00.
        let want = golden(0x1F, 0x02, 0x00, 0x04, &[mode_byte, 0x00]);
        assert_eq!(
            transport.sent()[0].as_slice(),
            want.as_slice(),
            "mode {mode_byte:#04x} frame differs from the hand-built golden"
        );
    }
}

/// The other four commands, each against a hand-built golden.
#[test]
fn every_command_family_matches_a_hand_built_golden_buffer() {
    struct Case {
        pid: u16,
        label: &'static str,
        request: RazerReport,
        want: [u8; 91],
    }

    let cases = [
        Case {
            pid: BLACKWIDOW_V4_PRO,
            label: "get_firmware_version (0x00/0x81, ds 2)",
            request: cmd::get_firmware_version(),
            want: golden(0x1F, 0x02, 0x00, 0x81, &[]),
        },
        Case {
            pid: BLACKWIDOW_V4_PRO,
            label: "get_serial (0x00/0x82, ds 0x16)",
            request: cmd::get_serial(),
            want: golden(0x1F, 0x16, 0x00, 0x82, &[]),
        },
        Case {
            pid: BLACKWIDOW_V4_PRO,
            label: "extended matrix brightness, BACKLIGHT_LED (0x0F/0x04, ds 3)",
            request: cmd::set_brightness(Storage::VarStore, LedId::Backlight, 0x80),
            want: golden(0x1F, 0x03, 0x0F, 0x04, &[0x01, 0x05, 0x80]),
        },
        Case {
            pid: BASILISK_WIRELESS,
            label: "extended matrix brightness, ZERO_LED (0x0F/0x04, ds 3)",
            request: cmd::set_brightness(Storage::VarStore, LedId::Zero, 0x80),
            want: golden(0x1F, 0x03, 0x0F, 0x04, &[0x01, 0x00, 0x80]),
        },
        Case {
            pid: BLACKWIDOW_V4_PRO,
            // razerchromacommon.c:505-509 spells the payload 010501000001ff0000.
            label: "extended matrix static effect (0x0F/0x02, ds 9)",
            request: cmd::set_static_effect(Storage::VarStore, LedId::Backlight, Rgb::new(0xFF, 0, 0)),
            want: golden(
                0x1F,
                0x09,
                0x0F,
                0x02,
                &[0x01, 0x05, 0x01, 0x00, 0x00, 0x01, 0xFF, 0x00, 0x00],
            ),
        },
        Case {
            pid: BLACKWIDOW_V4_PRO,
            label: "set_polling_rate2 8000 Hz (0x00/0x40, ds 2)",
            request: cmd::set_poll_rate_v2(PollRate::Hz8000, 0x00),
            want: golden(0x1F, 0x02, 0x00, 0x40, &[0x00, 0x01]),
        },
        Case {
            pid: BLACKWIDOW_V4_PRO,
            label: "get_polling_rate2 (0x00/0xC0, ds 1)",
            request: cmd::get_poll_rate_v2(),
            want: golden(0x1F, 0x01, 0x00, 0xC0, &[]),
        },
        Case {
            pid: BASILISK_WIRELESS,
            label: "set_polling_rate legacy 125 Hz (0x00/0x05, ds 1)",
            request: cmd::set_poll_rate_legacy(PollRate::Hz125).expect("125 Hz is legal"),
            want: golden(0x1F, 0x01, 0x00, 0x05, &[0x08]),
        },
        Case {
            pid: BASILISK_WIRELESS,
            label: "get_polling_rate legacy (0x00/0x85, ds 1)",
            request: cmd::get_poll_rate_legacy(),
            want: golden(0x1F, 0x01, 0x00, 0x85, &[]),
        },
    ];

    for case in cases {
        let mut transport = MockTransport::new();
        let mut clock = MockClock::new();
        {
            let mut s = Session::new(entry(case.pid), &mut transport, &mut clock);
            s.send(&case.request).expect("send");
        }
        assert_eq!(
            transport.sent()[0].as_slice(),
            case.want.as_slice(),
            "{} on {:#06x} does not match the hand-built golden",
            case.label,
            case.pid
        );
    }
}

// ===========================================================================
// 2. Poll-rate family — both directions
// ===========================================================================

/// Criterion 25 pins the SET ids. The GET ids are the mirror-image silent bug
/// and nothing else asserts them: `razer_chroma_misc_get_polling_rate` is
/// 0x00/0x85 and `..._polling_rate2` is 0x00/0xC0 (razerchromacommon.c:1092,
/// 1138). Reading with the wrong one returns a plausible-looking wrong rate.
#[test]
fn get_poll_rate_uses_the_right_command_id_for_each_family() {
    // BlackWidow V4 Pro reads with polling_rate2 — id 0xC0, code in arg[1].
    let mut transport = EchoTransport::new(&[0xFF, 0x08]);
    let mut clock = MockClock::new();
    let rate = {
        let mut s = Session::new(entry(BLACKWIDOW_V4_PRO), &mut transport, &mut clock);
        s.get_poll_rate().expect("v2 get")
    };
    assert_eq!(transport.sent[0][7], 0x00, "command class");
    assert_eq!(
        transport.sent[0][8], 0xC0,
        "the keyboard must use get_polling_rate2 (0xC0), not the legacy 0x85"
    );
    assert_eq!(transport.sent[0][6], 0x01, "data_size");
    assert_eq!(rate, PollRate::Hz1000, "v2 reads arguments[1]");

    // Basilisk V3 Pro reads with the legacy command — id 0x85, code in arg[0].
    for pid in [BASILISK_WIRED, BASILISK_WIRELESS] {
        let mut transport = EchoTransport::new(&[0x08, 0xFF]);
        let mut clock = MockClock::new();
        let rate = {
            let mut s = Session::new(entry(pid), &mut transport, &mut clock);
            s.get_poll_rate().expect("legacy get")
        };
        assert_eq!(
            transport.sent[0][8], 0x85,
            "{pid:#06x} must use the legacy get_polling_rate (0x85), not 0xC0"
        );
        assert_eq!(transport.sent[0][6], 0x01, "data_size");
        assert_eq!(rate, PollRate::Hz125, "legacy reads arguments[0]: 0x08 = 125 Hz");
    }
}

/// The wired Basilisk shares every poll-rate property with the wireless one.
/// Criterion 25 only exercises the wireless pid.
#[test]
fn the_wired_basilisk_uses_the_legacy_poll_rate_family_too() {
    let mut transport = EchoTransport::new(&[0x02]);
    let mut clock = MockClock::new();
    {
        let mut s = Session::new(entry(BASILISK_WIRED), &mut transport, &mut clock);
        s.set_poll_rate(PollRate::Hz500).expect("legacy set");
    }
    assert_eq!(transport.sent[0][7], 0x00, "command class");
    assert_eq!(transport.sent[0][8], 0x05, "legacy set id");
    assert_eq!(transport.sent[0][6], 0x01, "data_size");
    assert_eq!(transport.sent[0][9], 0x02, "500 Hz is 0x02 in legacy");
}

/// Criterion 27 pins the SET side of the LED id; the GET side is not asserted
/// anywhere. `razer_chroma_extended_matrix_get_brightness(VARSTORE, <led>)`
/// carries the same led byte, and reading the wrong zone reads the wrong lamp.
#[test]
fn get_brightness_uses_the_devices_own_led_id() {
    for (pid, led) in [
        (BLACKWIDOW_V4_PRO, 0x05u8), // BACKLIGHT_LED, razerkbd_driver.c:3993-4000
        (BASILISK_WIRELESS, 0x00),   // ZERO_LED, razermouse_driver.c:2337-2352
        (BASILISK_WIRED, 0x00),
    ] {
        let mut transport = EchoTransport::new(&[0x01, led, 0x33]);
        let mut clock = MockClock::new();
        let got = {
            let mut s = Session::new(entry(pid), &mut transport, &mut clock);
            s.get_brightness().expect("get brightness")
        };
        assert_eq!(transport.sent[0][7], 0x0F, "{pid:#06x}: command class");
        assert_eq!(transport.sent[0][8], 0x84, "{pid:#06x}: command id");
        assert_eq!(transport.sent[0][9], 0x01, "{pid:#06x}: VARSTORE");
        assert_eq!(transport.sent[0][10], led, "{pid:#06x}: LED id");
        assert_eq!(got, 0x33, "{pid:#06x}: brightness comes from arguments[2]");
    }
}

// ===========================================================================
// 3. Timing
// ===========================================================================

/// Criterion 20 only covers the wireless Basilisk. The wired pid falls into the
/// same `razer_get_report` case block (razermouse_driver.c:60-61) and therefore
/// takes the same `RAZER_NEW_MOUSE_RECEIVER_WAIT_US` of 31 000 us.
#[test]
fn the_wired_basilisk_also_waits_31ms() {
    let mut transport = MockTransport::new();
    let mut clock = MockClock::new();
    {
        let mut s = Session::new(entry(BASILISK_WIRED), &mut transport, &mut clock);
        s.send(&cmd::set_device_mode(DeviceMode::Driver))
            .expect("send");
    }
    assert_eq!(clock.sleeps(), [MOUSE_WAIT]);
}

/// Every device waits its own documented interval, checked against literals
/// rather than against `DeviceEntry::wait`.
#[test]
fn every_device_waits_its_own_documented_interval() {
    for entry in razer_devices::all() {
        let mut transport = MockTransport::new();
        let mut clock = MockClock::new();
        {
            let mut s = Session::new(entry, &mut transport, &mut clock);
            s.send(&cmd::get_firmware_version()).expect("send");
        }
        assert_eq!(
            clock.sleeps(),
            [required_wait(entry.pid)],
            "{} waited the wrong interval",
            entry.name
        );
    }
}

/// Criterion 21 counts sleeps by kind. Counting cannot tell "wait, retry, wait,
/// retry, ..." from "wait, wait, wait, wait, wait, retry, retry, retry, retry",
/// and only the first is the C driver's behaviour. Assert the exact sequence.
#[test]
fn the_retry_schedule_interleaves_device_waits_and_retry_delays() {
    let request = cmd::get_firmware_version();
    let mut transport = MockTransport::new();
    for _ in 0..MAX_ATTEMPTS {
        let mut wrong =
            RazerReport::new(0x0F, request.command_id, request.data_size).with_args(&[0x01, 0x02]);
        wrong.status = Status::Successful;
        transport.push_response(&wrong);
    }
    let mut clock = MockClock::new();
    let err = {
        let mut s = Session::new(entry(BLACKWIDOW_V4_PRO), &mut transport, &mut clock);
        s.transact(&request).expect_err("class mismatch")
    };
    assert!(matches!(err, HidError::RetriesExhausted { .. }), "{err:?}");

    // attempt 1: wait; then retry-delay, attempt 2: wait; ... 5 waits, 4 delays,
    // strictly alternating, starting and ending with a device wait.
    let expected = [
        KBD_WAIT,
        RETRY_DELAY,
        KBD_WAIT,
        RETRY_DELAY,
        KBD_WAIT,
        RETRY_DELAY,
        KBD_WAIT,
        RETRY_DELAY,
        KBD_WAIT,
    ];
    assert_eq!(clock.sleeps(), expected, "retry schedule out of order");
    assert_eq!(transport.sent().len(), MAX_ATTEMPTS);
    assert_all_frames_valid(transport.sent());
}

/// A mismatched `remaining_packets` alone must also be rejected — it is the
/// first of the three routing checks in `razer_send_payload()` and the easiest
/// to drop when porting.
#[test]
fn a_mismatched_remaining_packets_is_rejected_and_retried() {
    let request = cmd::get_firmware_version();
    let mut transport = MockTransport::new();
    for _ in 0..MAX_ATTEMPTS {
        let mut wrong =
            RazerReport::new(request.command_class, request.command_id, request.data_size);
        wrong.status = Status::Successful;
        wrong.remaining_packets = 1; // the only thing wrong with it
        transport.push_response(&wrong);
    }
    let mut clock = MockClock::new();
    let err = {
        let mut s = Session::new(entry(BLACKWIDOW_V4_PRO), &mut transport, &mut clock);
        s.transact(&request).expect_err("remaining_packets mismatch")
    };
    assert!(matches!(err, HidError::RetriesExhausted { .. }), "{err:?}");
    assert_eq!(transport.sent().len(), MAX_ATTEMPTS);
}

/// `RAZER_CMD_NOT_SUPPORTED` (0x05) and `RAZER_CMD_TIMEOUT` (0x04) are not
/// success. `razerkbd_driver.c:447-449` accepts only 0x02 and 0x01.
#[test]
fn only_successful_and_busy_count_as_success() {
    for status in [Status::Timeout, Status::NotSupported, Status::New] {
        let request = cmd::get_firmware_version();
        let mut transport = MockTransport::new();
        for _ in 0..MAX_ATTEMPTS {
            let mut r =
                RazerReport::new(request.command_class, request.command_id, request.data_size);
            r.status = status;
            transport.push_response(&r);
        }
        let mut clock = MockClock::new();
        let err = {
            let mut s = Session::new(entry(BLACKWIDOW_V4_PRO), &mut transport, &mut clock);
            s.transact(&request).unwrap_err()
        };
        assert!(
            matches!(err, HidError::RetriesExhausted { .. }),
            "status {status:?} must not be accepted as success, got {err:?}"
        );
        assert_eq!(
            transport.sent().len(),
            MAX_ATTEMPTS,
            "status {status:?} should have been retried"
        );
    }
}

// ===========================================================================
// 4. Transport failures
// ===========================================================================

/// Criterion 30 is only exercised with a zero-byte read. The realistic hidraw
/// short read is 90 — `usbhid_get_raw_report()` returns the 90 control-transfer
/// bytes plus 1 for the skipped report id, so anything less than 91 means the
/// device truncated its answer.
#[test]
fn a_ninety_byte_feature_read_is_still_a_short_read() {
    for got in [0usize, 1, 45, 89, 90] {
        let mut transport = FaultyGetTransport {
            sent: 0,
            outcome: Ok(got),
        };
        let mut clock = MockClock::new();
        let err = {
            let mut s = Session::new(entry(BLACKWIDOW_V4_PRO), &mut transport, &mut clock);
            s.transact_raw(&cmd::get_firmware_version())
                .expect_err("a truncated read must not be decoded")
        };
        match err {
            HidError::ShortRead { got: reported } => assert_eq!(reported, got),
            other => panic!("expected ShortRead{{got:{got}}}, got {other:?}"),
        }
    }
}

/// A full-length read is accepted — the boundary in the other direction, so the
/// short-read guard cannot be satisfied by rejecting everything.
#[test]
fn a_full_length_feature_read_is_accepted() {
    let mut transport = EchoTransport::new(&[0x01, 0x0B]);
    let mut clock = MockClock::new();
    let response = {
        let mut s = Session::new(entry(BLACKWIDOW_V4_PRO), &mut transport, &mut clock);
        s.transact_raw(&cmd::get_firmware_version())
            .expect("91 bytes is a complete report")
    };
    assert_eq!(response.arguments[0], 0x01);
    assert_eq!(response.arguments[1], 0x0B);
}

/// A failing `HIDIOCGFEATURE` must surface as `Io` naming that ioctl, not as a
/// short read or a protocol error.
#[test]
fn a_failing_get_feature_surfaces_the_errno_and_the_right_op() {
    let mut transport = FaultyGetTransport {
        sent: 0,
        outcome: Err(libc::EACCES),
    };
    let mut clock = MockClock::new();
    let err = {
        let mut s = Session::new(entry(BLACKWIDOW_V4_PRO), &mut transport, &mut clock);
        s.transact(&cmd::get_firmware_version())
            .expect_err("the read failed")
    };
    assert!(
        matches!(
            err,
            HidError::Io {
                op: "HIDIOCGFEATURE",
                errno: 13
            }
        ),
        "got {err:?}"
    );
    assert_eq!(
        transport.sent, 1,
        "a transport fault must not be hammered five times"
    );
}

/// A short read must not be retried either — the documented deviation from
/// `razer_send_payload()`, pinned so it cannot drift silently.
#[test]
fn a_short_read_is_not_retried() {
    let mut transport = FaultyGetTransport {
        sent: 0,
        outcome: Ok(0),
    };
    let mut clock = MockClock::new();
    let err = {
        let mut s = Session::new(entry(BLACKWIDOW_V4_PRO), &mut transport, &mut clock);
        s.transact(&cmd::get_firmware_version())
            .expect_err("nothing to read")
    };
    assert!(matches!(err, HidError::ShortRead { .. }), "got {err:?}");
    assert_eq!(transport.sent, 1, "exactly one attempt");
    assert!(
        !clock.sleeps().contains(&RETRY_DELAY),
        "no retry delay should have been taken"
    );
}

// ===========================================================================
// 5. Enumeration
// ===========================================================================

/// A different vendor's device that happens to share a Razer product id must
/// not be opened. Matching on pid+interface alone would open a stranger's
/// hidraw node and write 91 bytes of Razer protocol into it.
#[test]
fn a_foreign_vendor_sharing_the_product_id_is_not_matched() {
    let sysfs = MockSysfs::new()
        .with_node("hidraw0", info(0x046D, BLACKWIDOW_V4_PRO, 3, 0x02))
        .with_node("hidraw1", info(0x0000, BLACKWIDOW_V4_PRO, 3, 0x02));
    let err = find_device(&sysfs, RAZER, BLACKWIDOW_V4_PRO, 3)
        .expect_err("no Razer-vendor node is present");
    assert!(matches!(err, HidError::DeviceNotFound { .. }), "{err:?}");

    // And with the real vendor present it is found, so the guard is not just
    // rejecting everything.
    let sysfs = sysfs.with_node("hidraw2", info(RAZER, BLACKWIDOW_V4_PRO, 3, 0x02));
    let node = find_device(&sysfs, RAZER, BLACKWIDOW_V4_PRO, 3).expect("the Razer node");
    assert_eq!(node.name, "hidraw2");
    assert_eq!(node.info.vendor_id, RAZER);
}

/// The hidraw number and the interface number are independent. A fixture where
/// they disagree catches an implementation that parses `hidrawN` and uses `N`.
#[test]
fn the_hidraw_number_is_not_the_interface_number() {
    let sysfs = MockSysfs::new()
        .with_node("hidraw3", info(RAZER, BLACKWIDOW_V4_PRO, 0, 0x01))
        .with_node("hidraw7", info(RAZER, BLACKWIDOW_V4_PRO, 3, 0x02));
    let node = find_device(&sysfs, RAZER, BLACKWIDOW_V4_PRO, 3).expect("interface 3 exists");
    assert_eq!(
        node.name, "hidraw7",
        "interface 3 lives on hidraw7 in this fixture; hidraw3 is interface 0"
    );
    assert_eq!(node.path, std::path::PathBuf::from("/dev/hidraw7"));
}

/// `interface_protocol` is descriptive metadata, not the selection key. A
/// fixture where the requested interface number carries a *different* protocol
/// from the 0x02 node proves `find_device` keys on `bInterfaceNumber` — the
/// `wIndex` of the C driver's `USB_RECIP_INTERFACE` transfer — and not on a
/// protocol byte that merely happens to correlate on the two devices in scope.
#[test]
fn selection_keys_on_the_interface_number_not_the_interface_protocol() {
    let sysfs = MockSysfs::new()
        // The 0x02 ("mouse protocol") node, but on interface 1.
        .with_node("hidraw0", info(RAZER, BLACKWIDOW_V4_PRO, 1, 0x02))
        // The node the report index actually names, carrying protocol 0x00.
        .with_node("hidraw1", info(RAZER, BLACKWIDOW_V4_PRO, 3, 0x00));

    let node = find_device(&sysfs, RAZER, BLACKWIDOW_V4_PRO, 3).expect("interface 3 exists");
    assert_eq!(
        node.name, "hidraw1",
        "find_device followed interface_protocol 0x02 instead of interface_number 3"
    );
    assert_eq!(node.info.interface_number, 3);

    // And an interface number with no node at all is not found, even though a
    // protocol-0x02 node is sitting right there.
    let err = find_device(&sysfs, RAZER, BLACKWIDOW_V4_PRO, 2)
        .expect_err("there is no interface 2 in this fixture");
    assert!(matches!(err, HidError::DeviceNotFound { interface: 2, .. }), "{err:?}");
}

/// A `/sys` that cannot be listed must fail loudly, not report "device not
/// found" — the two send the user looking in completely different places.
#[test]
fn a_sysfs_listing_failure_is_not_reported_as_device_not_found() {
    let sysfs = BrokenSysfs { fail_listing: true };
    let err = find_device(&sysfs, RAZER, BLACKWIDOW_V4_PRO, 3).expect_err("the listing failed");
    assert!(
        matches!(err, HidError::Io { errno: 13, .. }),
        "expected the EACCES to survive, got {err:?}"
    );

    let err = list_razer_nodes(&sysfs).expect_err("the listing failed");
    assert!(matches!(err, HidError::Io { errno: 13, .. }), "{err:?}");
}

/// Documents a real robustness weakness rather than asserting it is good: one
/// unparseable attribute on ANY node aborts the whole scan, so an unrelated
/// device can hide the keyboard. See the report accompanying these tests.
#[test]
fn one_unreadable_node_currently_aborts_the_whole_scan() {
    let sysfs = BrokenSysfs {
        fail_listing: false,
    };
    let err = find_device(&sysfs, RAZER, BLACKWIDOW_V4_PRO, 3).expect_err("interface_info errors");
    assert!(
        matches!(err, HidError::Sysfs(_)),
        "expected the sysfs parse error to propagate, got {err:?}"
    );
}

/// Determinism, but with the matching nodes on DIFFERENT interfaces so the
/// answer is uniquely determined by the interface number and the tie-break is
/// never reached — a stronger statement than "the same node came back twice".
#[test]
fn selection_is_stable_across_a_reshuffled_source() {
    for order in [
        ["hidraw2", "hidraw11", "hidraw0"],
        ["hidraw11", "hidraw0", "hidraw2"],
        ["hidraw0", "hidraw2", "hidraw11"],
    ] {
        let mut sysfs = MockSysfs::new();
        for name in order {
            let iface = match name {
                "hidraw0" => 0,
                "hidraw2" => 1,
                _ => 3,
            };
            let proto = if iface == 3 { 0x02 } else { 0x01 };
            sysfs = sysfs.with_node(name, info(RAZER, BLACKWIDOW_V4_PRO, iface, proto));
        }
        let node = find_device(&sysfs, RAZER, BLACKWIDOW_V4_PRO, 3).expect("interface 3");
        assert_eq!(node.name, "hidraw11", "order {order:?} changed the answer");
        assert_eq!(node.info.interface_protocol, 0x02);
    }
}

// ===========================================================================
// 6. Guards
// ===========================================================================

/// A `POLL_RATE` capability with no other capability set still guards the
/// others — the bitmask must be tested per flag, not "is it non-zero".
static ONLY_POLL_RATE: DeviceEntry = DeviceEntry {
    pid: 0xDEA1,
    name: "synthetic: poll rate only",
    kind: DeviceKind::Mouse,
    transaction_id: 0x1F,
    report_index: 0x00,
    response_index: 0x00,
    wait: Duration::from_micros(31_000),
    capabilities: Capabilities(Capabilities::POLL_RATE.bits()),
    default_led: 0x00,
    poll_rate_kind: PollRateKind::Legacy,
};

/// An entry whose `default_led` is not one of the modelled ids.
static WEIRD_LED: DeviceEntry = DeviceEntry {
    pid: 0xDEA2,
    name: "synthetic: unmodelled led id",
    kind: DeviceKind::Keyboard,
    transaction_id: 0x1F,
    report_index: 0x03,
    response_index: 0x03,
    wait: Duration::from_micros(600),
    capabilities: Capabilities(
        Capabilities::BRIGHTNESS.bits() | Capabilities::STATIC_EFFECT.bits(),
    ),
    default_led: 0x07,
    poll_rate_kind: PollRateKind::V2,
};

#[test]
fn capabilities_are_checked_per_flag_not_as_a_yes_no() {
    let mut transport = MockTransport::new();
    let mut clock = MockClock::new();
    {
        let mut s = Session::new(&ONLY_POLL_RATE, &mut transport, &mut clock);
        assert!(
            matches!(
                s.set_device_mode(DeviceMode::Driver),
                Err(HidError::Unsupported { what: "device mode", .. })
            ),
            "DEVICE_MODE is not set on this entry"
        );
        assert!(s.firmware_version().is_err());
        assert!(s.serial().is_err());
        assert!(s.set_brightness(0x10).is_err());
        assert!(s.set_static_effect(Rgb::new(1, 2, 3)).is_err());
    }
    assert!(transport.sent().is_empty(), "nothing may reach the wire");
    assert!(clock.sleeps().is_empty());

    // ...but the one capability it does have works.
    let mut transport = EchoTransport::new(&[0x01]);
    let mut clock = MockClock::new();
    {
        let mut s = Session::new(&ONLY_POLL_RATE, &mut transport, &mut clock);
        s.set_poll_rate(PollRate::Hz1000).expect("POLL_RATE is set");
    }
    assert_eq!(transport.sent.len(), 1);
}

/// An unmodelled LED id must be refused before the write, not encoded as
/// whatever `as u8` happens to produce.
#[test]
fn an_unmodelled_led_id_is_refused_before_anything_is_sent() {
    let mut transport = MockTransport::new();
    let mut clock = MockClock::new();
    {
        let mut s = Session::new(&WEIRD_LED, &mut transport, &mut clock);
        assert!(
            matches!(s.set_brightness(0x40), Err(HidError::Unsupported { .. })),
            "led 0x07 is not modelled"
        );
        assert!(s.get_brightness().is_err());
        assert!(s.set_static_effect(Rgb::new(0, 0, 0)).is_err());
    }
    assert!(
        transport.sent().is_empty(),
        "a frame with a guessed LED id must never leave"
    );
    assert!(clock.sleeps().is_empty());
}

/// The override is per-session and does not mutate the shared static table —
/// otherwise one diagnostic A/B run would poison every later session in the
/// process.
#[test]
fn the_transaction_id_override_does_not_leak_into_the_device_table() {
    {
        let mut transport = MockTransport::new();
        let mut clock = MockClock::new();
        let mut s = Session::new(entry(BLACKWIDOW_V4_PRO), &mut transport, &mut clock)
            .with_transaction_id_override(0xFF);
        s.send(&cmd::set_device_mode(DeviceMode::Driver))
            .expect("send");
    }
    assert_eq!(
        entry(BLACKWIDOW_V4_PRO).transaction_id,
        0x1F,
        "the static table was mutated"
    );

    let mut transport = MockTransport::new();
    let mut clock = MockClock::new();
    {
        let mut s = Session::new(entry(BLACKWIDOW_V4_PRO), &mut transport, &mut clock);
        s.send(&cmd::set_device_mode(DeviceMode::Driver))
            .expect("send");
    }
    assert_eq!(
        transport.sent()[0][2],
        0x1F,
        "a fresh session must not inherit the previous override"
    );
}

// ===========================================================================
// 7. Safety interlock
// ===========================================================================

/// `MockClock` must never actually block. If a real sleep leaked into the
/// session the mouse's 31 ms x 5 attempts would show up as wall time.
#[test]
fn the_mock_clock_never_actually_sleeps() {
    let start = std::time::Instant::now();
    let mut clock = MockClock::new();
    for _ in 0..1000 {
        clock.sleep(Duration::from_secs(60));
    }
    assert_eq!(clock.total(), Duration::from_secs(60_000));
    assert!(
        start.elapsed() < Duration::from_millis(500),
        "MockClock blocked for {:?}",
        start.elapsed()
    );
}

/// `MockSysfs` reports node paths only; nothing in this test binary can open
/// one. Pins that `device_path` is pure string work.
#[test]
fn mock_sysfs_paths_are_never_opened() {
    let nodes = list_razer_nodes(&MockSysfs::blackwidow_v4_pro()).expect("enumerates");
    assert_eq!(nodes.len(), 4);
    for n in &nodes {
        assert!(n.path.starts_with("/dev/"));
        assert_eq!(n.path, std::path::PathBuf::from("/dev").join(&n.name));
    }
}
