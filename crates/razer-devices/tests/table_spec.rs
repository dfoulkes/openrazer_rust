// SPDX-License-Identifier: GPL-2.0-or-later
//! Adversarial integration tests for `razer-devices`.
//!
//! These run against the PUBLIC API only, and deliberately assert POSITIVE
//! expected values transcribed by hand from the upstream C driver at
//! `the upstream OpenRazer `driver/` tree`, rather than re-deriving them from the
//! crate's own private constants. Several of the in-module tests are
//! self-referential (e.g. checking every entry contains `COMMON_CAPS`, which is
//! itself defined as exactly those flags — a tautology). The tests here close
//! that loop by pinning literals.
//!
//! No hardware is touched: this crate has no dependencies and no I/O surface.

use core::time::Duration;

use razer_devices::{Capabilities, DeviceKind, PollRateKind, VENDOR_ID, all, lookup};

// ---------------------------------------------------------------------------
// Hand-transcribed spec values. Sources cited per line.
// ---------------------------------------------------------------------------

/// razercommon.h:26 `#define USB_VENDOR_ID_RAZER 0x1532`
const SPEC_VENDOR_ID: u16 = 0x1532;

/// razermouse_driver.h:92 `USB_DEVICE_ID_RAZER_BASILISK_V3_PRO_WIRED 0x00AA`
const SPEC_BASILISK_WIRED: u16 = 0x00AA;
/// razermouse_driver.h:93 `USB_DEVICE_ID_RAZER_BASILISK_V3_PRO_WIRELESS 0x00AB`
const SPEC_BASILISK_WIRELESS: u16 = 0x00AB;
/// razerkbd_driver.h:99 `USB_DEVICE_ID_RAZER_BLACKWIDOW_V4_PRO 0x028D`
const SPEC_BLACKWIDOW: u16 = 0x028D;

/// razermouse_driver.h:131 `RAZER_NEW_MOUSE_RECEIVER_WAIT_US 31000`, selected
/// for both Basilisk V3 Pro PIDs by razer_get_report(), razermouse_driver.c:60-61
/// falling through to the `return razer_get_usb_response(..., RAZER_NEW_MOUSE_RECEIVER_WAIT_US)`
/// at razermouse_driver.c:81.
const SPEC_MOUSE_WAIT_US: u64 = 31_000;
/// razerkbd_driver.h:163 `RAZER_BLACKWIDOW_CHROMA_WAIT_US 600`, selected for the
/// V4 Pro by razer_get_report_params(), razerkbd_driver.c:334 -> 344.
const SPEC_KBD_WAIT_US: u64 = 600;

/// razerkbd_driver.c:342-343 `*report_index = 0x03; *response_index = 0x03;`
const SPEC_KBD_INDEX: u8 = 0x03;
/// razermouse_driver.c:36 `unsigned int index = 0;` — never reassigned on the
/// Basilisk V3 Pro path.
const SPEC_MOUSE_INDEX: u8 = 0x00;

/// The ONE correct transaction id for all three devices in scope.
/// Keyboard: razerkbd_driver.c:536 (case) -> 546 (`= 0x1F`).
/// Mouse:    razermouse_driver.c:3900-3901 (case) -> 3925 (`= 0x1f`).
const SPEC_TXID: u8 = 0x1F;

/// The value the buggy C `razer_attr_write_device_mode()` hardcodes at
/// razerkbd_driver.c:4308. Must never appear in our table.
const C_BUG_TXID: u8 = 0xFF;

/// razercommon.h:37 `#define ZERO_LED 0x00`
const SPEC_ZERO_LED: u8 = 0x00;
/// razercommon.h:41 `#define BACKLIGHT_LED 0x05`
const SPEC_BACKLIGHT_LED: u8 = 0x05;

/// Razer Kiyo Pro — a webcam, explicitly out of scope.
const KIYO_PRO: u16 = 0x0E05;

// ---------------------------------------------------------------------------
// Table shape
// ---------------------------------------------------------------------------

#[test]
fn vendor_id_is_exactly_the_c_constant() {
    assert_eq!(VENDOR_ID, SPEC_VENDOR_ID);
    assert_eq!(VENDOR_ID, 0x1532);
}

#[test]
fn table_length_is_exactly_three() {
    assert_eq!(
        all().len(),
        3,
        "table must hold exactly the 3 in-scope devices"
    );
}

#[test]
fn table_is_strictly_ascending_by_pid() {
    for w in all().windows(2) {
        assert!(
            w[0].pid < w[1].pid,
            "not strictly ascending: {:#06x} then {:#06x}",
            w[0].pid,
            w[1].pid
        );
    }
}

#[test]
fn table_pids_are_exactly_the_expected_set_in_order() {
    let pids: Vec<u16> = all().iter().map(|e| e.pid).collect();
    assert_eq!(
        pids,
        vec![SPEC_BASILISK_WIRED, SPEC_BASILISK_WIRELESS, SPEC_BLACKWIDOW]
    );
}

#[test]
fn table_names_are_unique() {
    for (i, a) in all().iter().enumerate() {
        for b in all().iter().skip(i + 1) {
            assert_ne!(a.name, b.name, "duplicate device name {:?}", a.name);
        }
    }
}

#[test]
fn table_holds_one_keyboard_and_two_mice() {
    let kbd = all()
        .iter()
        .filter(|e| e.kind == DeviceKind::Keyboard)
        .count();
    let mouse = all().iter().filter(|e| e.kind == DeviceKind::Mouse).count();
    assert_eq!(kbd, 1, "expected exactly one keyboard");
    assert_eq!(mouse, 2, "expected exactly two mice");
}

// ---------------------------------------------------------------------------
// Exact per-device values
// ---------------------------------------------------------------------------

#[test]
fn blackwidow_v4_pro_every_field_matches_the_c_source() {
    let e = lookup(SPEC_BLACKWIDOW).expect("0x028D must be present");
    assert_eq!(e.pid, 0x028D);
    assert_eq!(e.name, "Razer BlackWidow V4 Pro");
    assert!(e.name.contains("BlackWidow V4 Pro"));
    assert_eq!(e.kind, DeviceKind::Keyboard);
    assert_eq!(e.transaction_id, SPEC_TXID);
    assert_eq!(e.transaction_id, 0x1F);
    assert_eq!(e.report_index, SPEC_KBD_INDEX);
    assert_eq!(e.report_index, 3);
    assert_eq!(e.response_index, SPEC_KBD_INDEX);
    assert_eq!(e.response_index, 3);
    assert_eq!(e.wait, Duration::from_micros(SPEC_KBD_WAIT_US));
    assert_eq!(e.wait.as_micros(), 600);
    assert_eq!(e.default_led, SPEC_BACKLIGHT_LED);
    assert_eq!(e.default_led, 0x05);
    // razerkbd_driver.c:4622 uses razer_chroma_misc_set_polling_rate2 for the
    // V4 Pro, and :4553 the matching get2 — i.e. the v2 family.
    assert_eq!(e.poll_rate_kind, PollRateKind::V2);
}

#[test]
fn basilisk_v3_pro_wired_every_field_matches_the_c_source() {
    let e = lookup(SPEC_BASILISK_WIRED).expect("0x00AA must be present");
    assert_eq!(e.pid, 0x00AA);
    assert_eq!(e.name, "Razer Basilisk V3 Pro (Wired)");
    assert!(e.name.contains("Basilisk V3 Pro"));
    assert!(e.name.contains("Wired"));
    assert!(
        !e.name.contains("Wireless"),
        "wired entry must not say Wireless"
    );
    assert_eq!(e.kind, DeviceKind::Mouse);
    assert_eq!(e.transaction_id, SPEC_TXID);
    assert_eq!(e.report_index, SPEC_MOUSE_INDEX);
    assert_eq!(e.response_index, SPEC_MOUSE_INDEX);
    assert_eq!(e.wait, Duration::from_micros(SPEC_MOUSE_WAIT_US));
    assert_eq!(e.wait.as_micros(), 31_000);
    assert_eq!(e.default_led, SPEC_ZERO_LED);
    // razermouse_driver.c:2117 -> razer_chroma_misc_set_polling_rate (legacy).
    assert_eq!(e.poll_rate_kind, PollRateKind::Legacy);
}

#[test]
fn basilisk_v3_pro_wireless_every_field_matches_the_c_source() {
    let e = lookup(SPEC_BASILISK_WIRELESS).expect("0x00AB must be present");
    assert_eq!(e.pid, 0x00AB);
    assert_eq!(e.name, "Razer Basilisk V3 Pro (Wireless)");
    assert!(e.name.contains("Basilisk V3 Pro"));
    assert!(e.name.contains("Wireless"));
    assert_eq!(e.kind, DeviceKind::Mouse);
    assert_eq!(e.transaction_id, SPEC_TXID);
    assert_eq!(e.report_index, SPEC_MOUSE_INDEX);
    assert_eq!(e.response_index, SPEC_MOUSE_INDEX);
    assert_eq!(e.wait, Duration::from_micros(SPEC_MOUSE_WAIT_US));
    assert_eq!(e.default_led, SPEC_ZERO_LED);
    assert_eq!(e.poll_rate_kind, PollRateKind::Legacy);
}

#[test]
fn the_two_basilisk_entries_differ_only_in_pid_and_name() {
    let w = lookup(SPEC_BASILISK_WIRED).unwrap();
    let l = lookup(SPEC_BASILISK_WIRELESS).unwrap();
    assert_ne!(w.pid, l.pid);
    assert_ne!(w.name, l.name);
    assert_eq!(w.kind, l.kind);
    assert_eq!(w.transaction_id, l.transaction_id);
    assert_eq!(w.report_index, l.report_index);
    assert_eq!(w.response_index, l.response_index);
    assert_eq!(w.wait, l.wait);
    assert_eq!(w.default_led, l.default_led);
    assert_eq!(w.capabilities, l.capabilities);
    assert_eq!(w.poll_rate_kind, l.poll_rate_kind);
}

// ---------------------------------------------------------------------------
// Invariants across the whole table
// ---------------------------------------------------------------------------

#[test]
fn every_entry_uses_transaction_id_0x1f() {
    // Positive form: not merely "isn't 0xFF", but "is exactly 0x1F", which is
    // what both the keyboard (razerkbd_driver.c:546) and mouse
    // (razermouse_driver.c:3925) per-PID switches select for these devices.
    for e in all() {
        assert_eq!(e.transaction_id, SPEC_TXID, "{} has wrong txid", e.name);
    }
}

#[test]
fn no_entry_uses_the_buggy_0xff_transaction_id() {
    // Regression guard for the C driver defect this project exists to fix:
    // razer_attr_write_device_mode(), razerkbd_driver.c:4308.
    for e in all() {
        assert_ne!(
            e.transaction_id, C_BUG_TXID,
            "{} uses the buggy 0xFF",
            e.name
        );
    }
}

#[test]
fn report_index_equals_response_index_for_every_entry() {
    for e in all() {
        assert_eq!(
            e.report_index, e.response_index,
            "{}: report {:#04x} != response {:#04x}",
            e.name, e.report_index, e.response_index
        );
    }
}

#[test]
fn default_led_matches_device_kind() {
    for e in all() {
        let expected = match e.kind {
            // razerkbd_driver.c:4000 razer_chroma_extended_matrix_brightness(VARSTORE, BACKLIGHT_LED, ..)
            DeviceKind::Keyboard => SPEC_BACKLIGHT_LED,
            // razermouse_driver.c:2281 razer_chroma_extended_matrix_brightness(VARSTORE, ZERO_LED, ..)
            DeviceKind::Mouse => SPEC_ZERO_LED,
        };
        assert_eq!(e.default_led, expected, "{} default_led", e.name);
    }
}

#[test]
fn poll_rate_kind_matches_device_kind() {
    for e in all() {
        let expected = match e.kind {
            DeviceKind::Keyboard => PollRateKind::V2,
            DeviceKind::Mouse => PollRateKind::Legacy,
        };
        assert_eq!(e.poll_rate_kind, expected, "{} poll_rate_kind", e.name);
    }
}

#[test]
fn no_entry_has_a_zero_wait() {
    for e in all() {
        assert!(
            e.wait > Duration::ZERO,
            "{} must have a non-zero post-SET_REPORT wait",
            e.name
        );
    }
}

// ---------------------------------------------------------------------------
// lookup() — including the exhaustive sweep that stops scope creep dead
// ---------------------------------------------------------------------------

#[test]
fn lookup_round_trips_every_entry() {
    for e in all() {
        let got = lookup(e.pid).expect("every table entry must be findable");
        assert_eq!(got.pid, e.pid);
        assert_eq!(
            got, e,
            "lookup returned a different entry for {:#06x}",
            e.pid
        );
    }
}

#[test]
fn exhaustive_sweep_of_the_entire_u16_pid_space() {
    // The strongest possible statement of "only these three devices". Catches a
    // stray fourth entry, a duplicated pid, a typo'd pid, and any lookup()
    // that matches more loosely than exact equality.
    let mut found = Vec::new();
    for pid in 0u16..=u16::MAX {
        if let Some(e) = lookup(pid) {
            assert_eq!(
                e.pid, pid,
                "lookup({:#06x}) returned pid {:#06x}",
                pid, e.pid
            );
            found.push(pid);
        }
    }
    assert_eq!(
        found,
        vec![SPEC_BASILISK_WIRED, SPEC_BASILISK_WIRELESS, SPEC_BLACKWIDOW],
        "exactly three pids in the whole 16-bit space may resolve"
    );
}

#[test]
fn kiyo_pro_webcam_is_absent() {
    assert!(
        lookup(KIYO_PRO).is_none(),
        "0x0E05 is a webcam, out of scope"
    );
}

#[test]
fn neighbouring_and_sentinel_pids_return_none() {
    for pid in [
        0x0000u16, 0xFFFF, 0x028C, 0x028E, 0x00A9, 0x00AC, 0x0E05,
        // Other real Razer PIDs the C driver supports but we deliberately do not:
        0x00CC, // BASILISK_V3_PRO_35K_WIRED, razermouse_driver.h:114
        0x00CD, // BASILISK_V3_PRO_35K_WIRELESS, razermouse_driver.h:115
        0x1532, // the vendor id, in case anyone confuses the two
    ] {
        assert!(lookup(pid).is_none(), "lookup({pid:#06x}) must be None");
    }
}

// ---------------------------------------------------------------------------
// Capabilities
// ---------------------------------------------------------------------------

#[test]
fn capability_flags_have_exact_pinned_bit_values() {
    // Positive assertion of the encoding, not just "distinct and power of two".
    assert_eq!(Capabilities::NONE.bits(), 0b000000);
    assert_eq!(Capabilities::DEVICE_MODE.bits(), 0b000001);
    assert_eq!(Capabilities::FIRMWARE_VERSION.bits(), 0b000010);
    assert_eq!(Capabilities::SERIAL.bits(), 0b000100);
    assert_eq!(Capabilities::BRIGHTNESS.bits(), 0b001000);
    assert_eq!(Capabilities::STATIC_EFFECT.bits(), 0b010000);
    assert_eq!(Capabilities::POLL_RATE.bits(), 0b100000);
}

#[test]
fn all_six_flags_are_distinct_powers_of_two() {
    let flags = [
        Capabilities::DEVICE_MODE,
        Capabilities::FIRMWARE_VERSION,
        Capabilities::SERIAL,
        Capabilities::BRIGHTNESS,
        Capabilities::STATIC_EFFECT,
        Capabilities::POLL_RATE,
    ];
    for f in flags {
        assert_eq!(
            f.bits().count_ones(),
            1,
            "{:#x} not a power of two",
            f.bits()
        );
    }
    for (i, a) in flags.iter().enumerate() {
        for b in flags.iter().skip(i + 1) {
            assert_ne!(a.bits(), b.bits());
        }
    }
}

#[test]
fn none_contains_nothing_but_itself() {
    assert!(!Capabilities::NONE.contains(Capabilities::SERIAL));
    assert!(!Capabilities::NONE.contains(Capabilities::DEVICE_MODE));
    assert!(!Capabilities::NONE.contains(Capabilities::FIRMWARE_VERSION));
    assert!(!Capabilities::NONE.contains(Capabilities::BRIGHTNESS));
    assert!(!Capabilities::NONE.contains(Capabilities::STATIC_EFFECT));
    assert!(!Capabilities::NONE.contains(Capabilities::POLL_RATE));
    assert!(Capabilities::NONE.contains(Capabilities::NONE));
}

#[test]
fn union_contains_its_members_and_nothing_else() {
    let c = Capabilities::SERIAL | Capabilities::BRIGHTNESS;
    assert!(c.contains(Capabilities::SERIAL));
    assert!(c.contains(Capabilities::BRIGHTNESS));
    assert!(c.contains(Capabilities::SERIAL | Capabilities::BRIGHTNESS));
    assert!(!c.contains(Capabilities::POLL_RATE));
    assert!(!c.contains(Capabilities::DEVICE_MODE));
    assert!(!c.contains(Capabilities::FIRMWARE_VERSION));
    assert!(!c.contains(Capabilities::STATIC_EFFECT));
    assert_eq!(c.bits(), 0b001100);
}

#[test]
fn union_is_commutative_idempotent_and_absorbs_none() {
    let a = Capabilities::DEVICE_MODE;
    let b = Capabilities::POLL_RATE;
    assert_eq!((a | b).bits(), (b | a).bits());
    assert_eq!((a | a).bits(), a.bits());
    assert_eq!((a | Capabilities::NONE).bits(), a.bits());
    assert_eq!(a.union(b).bits(), (a | b).bits());
}

#[test]
fn contains_is_reflexive_over_every_entry() {
    for e in all() {
        assert!(e.capabilities.contains(e.capabilities));
        assert!(e.capabilities.contains(Capabilities::NONE));
    }
}

#[test]
fn every_entry_declares_the_six_required_capabilities_by_literal() {
    // Deliberately NOT written against the crate's private COMMON_CAPS: that
    // test is a tautology. This one names each of the six flags from the
    // acceptance criteria directly.
    for e in all() {
        assert!(
            e.capabilities.contains(Capabilities::DEVICE_MODE),
            "{}",
            e.name
        );
        assert!(
            e.capabilities.contains(Capabilities::FIRMWARE_VERSION),
            "{}",
            e.name
        );
        assert!(e.capabilities.contains(Capabilities::SERIAL), "{}", e.name);
        assert!(
            e.capabilities.contains(Capabilities::BRIGHTNESS),
            "{}",
            e.name
        );
        assert!(
            e.capabilities.contains(Capabilities::STATIC_EFFECT),
            "{}",
            e.name
        );
        assert!(
            e.capabilities.contains(Capabilities::POLL_RATE),
            "{}",
            e.name
        );
    }
}

#[test]
fn no_entry_declares_capabilities_beyond_the_documented_six() {
    let known = Capabilities::DEVICE_MODE
        | Capabilities::FIRMWARE_VERSION
        | Capabilities::SERIAL
        | Capabilities::BRIGHTNESS
        | Capabilities::STATIC_EFFECT
        | Capabilities::POLL_RATE;
    for e in all() {
        assert_eq!(
            e.capabilities.bits() & !known.bits(),
            0,
            "{} declares an undefined capability bit: {:#x}",
            e.name,
            e.capabilities.bits()
        );
        // And since it also contains all six, it is exactly the six.
        assert_eq!(e.capabilities.bits(), 0b111111, "{}", e.name);
    }
}

// ---------------------------------------------------------------------------
// Hardware-safety: the crate is pure data. Nothing here can reach a device.
// ---------------------------------------------------------------------------

#[test]
fn all_returns_a_stable_static_slice() {
    // `all()` must hand back the same static storage every call — no allocation,
    // no interior mutability, nothing that could be swapped at runtime.
    let a = all();
    let b = all();
    assert_eq!(a.as_ptr(), b.as_ptr());
    assert_eq!(a.len(), b.len());
}
