// SPDX-License-Identifier: GPL-2.0-or-later
//! Enforces `tests/fixtures/golden_vectors.txt`.
//!
//! That file carries the exact request frames the Phase 3 experiment
//! (`docs/phase3-experiment.md`) puts on the wire by hand, against Dan's only
//! keyboard. It claimed the crate "MUST reproduce these byte-for-byte" while
//! nothing checked it, so a drifting constructor would have silently
//! invalidated the experiment rather than failing a test. It is checked now.
//!
//! The fixture is embedded with [`include_str!`], which is resolved by the
//! compiler. There is **no runtime I/O here**: no file is opened, no path is
//! touched, and razer-proto keeps its "pure, no-I/O, no-dependencies" property
//! intact. The expected bytes come from the fixture; the actual bytes are
//! rebuilt from the public constructors, so the two are independent.

use razer_proto::{DeviceMode, REPORT_LEN, RazerReport, cmd, verify_crc};

const FIXTURE: &str = include_str!("fixtures/golden_vectors.txt");

/// Pull one named vector out of the fixture, as raw bytes.
///
/// Sections are `## name`, followed by comment lines, followed by one line of
/// hex. Panics rather than returning an error: a missing or malformed vector
/// means the fixture has been damaged, and that must stop the suite.
fn vector(name: &str) -> [u8; REPORT_LEN] {
    let header = format!("## {name}");
    let mut lines = FIXTURE
        .lines()
        .skip_while(|l| l.trim() != header)
        .skip(1)
        .skip_while(|l| l.trim_start().starts_with('#') || l.trim().is_empty());

    let hex = lines
        .next()
        .unwrap_or_else(|| panic!("fixture has no payload line for `{name}`"))
        .trim();

    assert_eq!(
        hex.len(),
        REPORT_LEN * 2,
        "`{name}` must be {REPORT_LEN} bytes of hex, got {} chars",
        hex.len()
    );

    std::array::from_fn(|i| {
        u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
            .unwrap_or_else(|e| panic!("`{name}` byte {i} is not hex: {e}"))
    })
}

/// Assert a constructed report reproduces a fixture vector exactly, and report
/// the first differing index rather than dumping two 90-byte blobs.
fn assert_matches(name: &str, built: RazerReport) {
    let want = vector(name);
    let got = built.to_bytes();

    if let Some(i) = (0..REPORT_LEN).find(|&i| want[i] != got[i]) {
        panic!(
            "`{name}` differs at byte {i}: fixture {:#04x}, constructed {:#04x}\n\
             fixture:     {}\n\
             constructed: {}",
            want[i],
            got[i],
            want.iter().map(|b| format!("{b:02x}")).collect::<String>(),
            got.iter().map(|b| format!("{b:02x}")).collect::<String>(),
        );
    }
}

#[test]
fn every_vector_is_ninety_bytes_and_checksum_valid() {
    for name in [
        "firmware_version_v4pro",
        "serial_v4pro",
        "device_mode_driver_correct",
        "device_mode_driver_buggy",
        "device_mode_device_correct",
    ] {
        // The 90-byte length is enforced inside `vector()`, against the hex
        // text itself — by the time it returns an array it is too late to check.
        let bytes = vector(name);
        assert!(
            verify_crc(&bytes),
            "`{name}` carries a checksum the device would accept"
        );
    }
}

#[test]
fn firmware_version_vector_matches_the_constructor() {
    assert_matches(
        "firmware_version_v4pro",
        cmd::get_firmware_version().with_transaction_id(0x1F),
    );
}

#[test]
fn serial_vector_matches_the_constructor() {
    assert_matches("serial_v4pro", cmd::get_serial().with_transaction_id(0x1F));
}

/// Phase 3 arm C — the frame upstream *should* send. `razerkbd_driver.c:536`
/// lists the BlackWidow V4 Pro in the arm that sets `0x1F` at line 546.
#[test]
fn phase3_arm_c_is_the_correct_driver_mode_frame() {
    assert_matches(
        "device_mode_driver_correct",
        cmd::set_device_mode(DeviceMode::Driver).with_transaction_id(0x1F),
    );
}

/// Phase 3 arm B — the frame upstream *actually* sends.
/// `razer_attr_write_device_mode()` hardcodes `0xFF` at `razerkbd_driver.c:4308`.
/// This is the suspected fault, reproduced deliberately.
#[test]
fn phase3_arm_b_is_the_upstream_buggy_frame() {
    assert_matches(
        "device_mode_driver_buggy",
        cmd::set_device_mode(DeviceMode::Driver).with_transaction_id(0xFF),
    );
}

#[test]
fn returning_to_normal_mode_matches_the_constructor() {
    assert_matches(
        "device_mode_device_correct",
        cmd::set_device_mode(DeviceMode::Normal).with_transaction_id(0x1F),
    );
}

/// The experiment's whole premise: arms B and C differ at exactly one byte —
/// the transaction id — and *both* checksums are valid, because the checksum
/// covers bytes 2..=87 and the transaction id is byte 1. The device has no way
/// to reject the malformed frame, which is why the fault is silent.
#[test]
fn the_two_experiment_arms_differ_only_at_the_transaction_id() {
    let good = vector("device_mode_driver_correct");
    let bad = vector("device_mode_driver_buggy");

    let differing: Vec<usize> = (0..REPORT_LEN).filter(|&i| good[i] != bad[i]).collect();
    assert_eq!(differing, vec![1], "only byte 1 may differ");

    assert_eq!(good[1], 0x1F, "arm C uses the per-device transaction id");
    assert_eq!(bad[1], 0xFF, "arm B reproduces razerkbd_driver.c:4308");
    assert_eq!(good[88], bad[88], "the checksum cannot see the difference");
    assert!(verify_crc(&good) && verify_crc(&bad));
}

/// No vector may claim a transaction id of `0x00`: `razer_get_usb_response()`
/// (`razercommon.c:68-70`) silently rewrites `0x00` to `0xFF`, which would
/// turn any such vector into arm B behind the experimenter's back.
#[test]
fn no_vector_leaves_the_transaction_id_unstamped() {
    for name in [
        "firmware_version_v4pro",
        "serial_v4pro",
        "device_mode_driver_correct",
        "device_mode_device_correct",
    ] {
        assert_ne!(vector(name)[1], 0x00, "`{name}` must stamp a real tid");
        assert_ne!(vector(name)[1], 0xFF, "`{name}` must not use the buggy tid");
    }
}
