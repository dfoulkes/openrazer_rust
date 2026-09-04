// SPDX-License-Identifier: GPL-2.0-or-later
//! Adversarial wire-format tests, written against the C source as the
//! specification rather than against the Rust implementation.
//!
//! Every expected frame in this file was built **by hand** from
//! `the upstream OpenRazer `driver/` tree`, and every checksum was computed by
//! hand as the XOR of the non-zero bytes in the covered range 2..=87. Nothing
//! here calls `crc()` to produce an expectation — that would just assert the
//! implementation agrees with itself.
//!
//! NO HARDWARE IS TOUCHED. This crate has no I/O of any kind; these are
//! in-memory byte fixtures.

use razer_proto::{
    ARGS_LEN, DeviceMode, LedId, PollRate, ProtoError, REPORT_LEN, RazerReport, Rgb, Status,
    Storage, cmd, crc, parse, verify_crc,
};

/// Build the expected 90-byte frame from its spec fields.
///
/// `crc_byte` is supplied by the caller as a hand-computed constant. This
/// helper deliberately does **not** compute it.
fn expect_frame(
    transaction_id: u8,
    data_size: u8,
    command_class: u8,
    command_id: u8,
    args: &[u8],
    crc_byte: u8,
) -> [u8; REPORT_LEN] {
    let mut b = [0u8; REPORT_LEN];
    b[0] = 0x00; // status: new command (razercommon.c:132)
    b[1] = transaction_id;
    b[2] = 0x00; // remaining_packets, big-endian high byte
    b[3] = 0x00; // remaining_packets, big-endian low byte
    b[4] = 0x00; // protocol_type is always 0x00
    b[5] = data_size;
    b[6] = command_class;
    b[7] = command_id;
    b[8..8 + args.len()].copy_from_slice(args);
    b[88] = crc_byte;
    b[89] = 0x00; // reserved
    b
}

/// XOR-fold, spelled out, so a reader can check the constants by eye.
fn xor(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |a, b| a ^ b)
}

// ---------------------------------------------------------------------------
// Hand-computed golden frames, one per constructor.
//
// Transaction id 0x1F throughout: the BlackWidow V4 Pro's value from
// razerkbd_driver.c:536/546. It is stamped by the caller, never by cmd.
// ---------------------------------------------------------------------------

#[test]
fn golden_set_device_mode_normal() {
    // razerchromacommon.c:25-44 -> class 0x00, id 0x04, data_size 0x02,
    // arguments [0x00, 0x00].
    // crc = 0x02 ^ 0x04 = 0x06
    let expected = expect_frame(0x1F, 0x02, 0x00, 0x04, &[0x00, 0x00], 0x06);
    assert_eq!(xor(&[0x02, 0x04]), 0x06, "hand crc");
    let got = cmd::set_device_mode(DeviceMode::Normal)
        .with_transaction_id(0x1F)
        .to_bytes();
    assert_eq!(got, expected);
}

#[test]
fn golden_set_device_mode_driver() {
    // The frame this entire project exists to get right.
    // crc = 0x02 ^ 0x04 ^ 0x03 = 0x05
    let expected = expect_frame(0x1F, 0x02, 0x00, 0x04, &[0x03, 0x00], 0x05);
    assert_eq!(xor(&[0x02, 0x04, 0x03]), 0x05, "hand crc");
    let got = cmd::set_device_mode(DeviceMode::Driver)
        .with_transaction_id(0x1F)
        .to_bytes();
    assert_eq!(got, expected);
}

#[test]
fn golden_get_device_mode() {
    // razerchromacommon.c:51-54. crc = 0x02 ^ 0x84 = 0x86
    let expected = expect_frame(0x1F, 0x02, 0x00, 0x84, &[], 0x86);
    assert_eq!(xor(&[0x02, 0x84]), 0x86, "hand crc");
    assert_eq!(
        cmd::get_device_mode().with_transaction_id(0x1F).to_bytes(),
        expected
    );
}

#[test]
fn golden_get_firmware_version() {
    // razerchromacommon.c:67-70. crc = 0x02 ^ 0x81 = 0x83
    let expected = expect_frame(0x1F, 0x02, 0x00, 0x81, &[], 0x83);
    assert_eq!(xor(&[0x02, 0x81]), 0x83, "hand crc");
    assert_eq!(
        cmd::get_firmware_version()
            .with_transaction_id(0x1F)
            .to_bytes(),
        expected
    );
}

#[test]
fn golden_get_serial() {
    // razerchromacommon.c:59-62. data_size 0x16 == 22, the serial length.
    // crc = 0x16 ^ 0x82 = 0x94
    let expected = expect_frame(0x1F, 0x16, 0x00, 0x82, &[], 0x94);
    assert_eq!(xor(&[0x16, 0x82]), 0x94, "hand crc");
    assert_eq!(
        cmd::get_serial().with_transaction_id(0x1F).to_bytes(),
        expected
    );
}

#[test]
fn golden_set_brightness_matches_the_c_doc_comment() {
    // razerchromacommon.c:709-713 documents the payload as `0104b7`:
    // VARSTORE (0x01), LOGO_LED (0x04), brightness 0xb7.
    // crc = 0x03 ^ 0x0F ^ 0x04 ^ 0x01 ^ 0x04 ^ 0xB7 = 0xBA
    let expected = expect_frame(0x1F, 0x03, 0x0F, 0x04, &[0x01, 0x04, 0xB7], 0xBA);
    assert_eq!(xor(&[0x03, 0x0F, 0x04, 0x01, 0x04, 0xB7]), 0xBA, "hand crc");
    let got = cmd::set_brightness(Storage::VarStore, LedId::Logo, 0xB7)
        .with_transaction_id(0x1F)
        .to_bytes();
    assert_eq!(got, expected);
}

#[test]
fn golden_get_brightness() {
    // razerchromacommon.c:731-738: data_size 0x03 but only two args written.
    // The third payload byte must be left ZERO on the wire.
    // crc = 0x03 ^ 0x0F ^ 0x84 ^ 0x01 ^ 0x05 = 0x8C
    let expected = expect_frame(0x1F, 0x03, 0x0F, 0x84, &[0x01, 0x05], 0x8C);
    assert_eq!(xor(&[0x03, 0x0F, 0x84, 0x01, 0x05]), 0x8C, "hand crc");
    let got = cmd::get_brightness(Storage::VarStore, LedId::Backlight)
        .with_transaction_id(0x1F)
        .to_bytes();
    assert_eq!(got, expected);
    assert_eq!(
        got[10], 0x00,
        "byte 10 is inside data_size but must be zero"
    );
}

#[test]
fn golden_set_static_effect_matches_the_c_doc_comment() {
    // razerchromacommon.c:505-509 documents `010501000001ff0000`.
    // crc = 0x09 ^ 0x0F ^ 0x02 ^ 0x01 ^ 0x05 ^ 0x01 ^ 0x01 ^ 0xFF = 0xFF
    let args = [0x01, 0x05, 0x01, 0x00, 0x00, 0x01, 0xFF, 0x00, 0x00];
    let expected = expect_frame(0x1F, 0x09, 0x0F, 0x02, &args, 0xFF);
    assert_eq!(
        xor(&[0x09, 0x0F, 0x02, 0x01, 0x05, 0x01, 0x01, 0xFF]),
        0xFF,
        "hand crc"
    );
    let got = cmd::set_static_effect(
        Storage::VarStore,
        LedId::Backlight,
        Rgb::new(0xFF, 0x00, 0x00),
    )
    .with_transaction_id(0x1F)
    .to_bytes();
    assert_eq!(got, expected);

    // The third line of the same comment: `010501000001008000` (RGB 0x008000).
    let dim_green = cmd::set_static_effect(
        Storage::VarStore,
        LedId::Backlight,
        Rgb::new(0x00, 0x80, 0x00),
    )
    .to_bytes();
    assert_eq!(
        &dim_green[8..17],
        &[0x01, 0x05, 0x01, 0x00, 0x00, 0x01, 0x00, 0x80, 0x00]
    );
}

#[test]
fn golden_set_effect_none_matches_the_c_doc_comment() {
    // razerchromacommon.c:717-721 documents `010500000000`, data_size 0x06.
    // crc = 0x06 ^ 0x0F ^ 0x02 ^ 0x01 ^ 0x05 = 0x0F
    let args = [0x01, 0x05, 0x00, 0x00, 0x00, 0x00];
    let expected = expect_frame(0x1F, 0x06, 0x0F, 0x02, &args, 0x0F);
    assert_eq!(xor(&[0x06, 0x0F, 0x02, 0x01, 0x05]), 0x0F, "hand crc");
    let got = cmd::set_effect_none(Storage::VarStore, LedId::Backlight)
        .with_transaction_id(0x1F)
        .to_bytes();
    assert_eq!(got, expected);
}

#[test]
fn golden_poll_rate_legacy_frames() {
    // razerchromacommon.c:1104-1123: class 0x00, id 0x05, data_size 0x01.
    // crc = 0x01 ^ 0x05 ^ code
    for (rate, code, crc_byte) in [
        (PollRate::Hz1000, 0x01u8, 0x05u8),
        (PollRate::Hz500, 0x02, 0x06),
        (PollRate::Hz125, 0x08, 0x0C),
    ] {
        assert_eq!(xor(&[0x01, 0x05, code]), crc_byte, "hand crc for {rate:?}");
        let expected = expect_frame(0x1F, 0x01, 0x00, 0x05, &[code], crc_byte);
        let got = cmd::set_poll_rate_legacy(rate)
            .expect("legacy-encodable")
            .with_transaction_id(0x1F)
            .to_bytes();
        assert_eq!(got, expected, "{rate:?}");
    }
}

#[test]
fn golden_get_poll_rate_legacy() {
    // razerchromacommon.c:1092-1095. crc = 0x01 ^ 0x85 = 0x84
    let expected = expect_frame(0x1F, 0x01, 0x00, 0x85, &[], 0x84);
    assert_eq!(xor(&[0x01, 0x85]), 0x84, "hand crc");
    assert_eq!(
        cmd::get_poll_rate_legacy()
            .with_transaction_id(0x1F)
            .to_bytes(),
        expected
    );
}

#[test]
fn golden_poll_rate_v2_frames() {
    // razerchromacommon.c:1153-1189: class 0x00, id 0x40, data_size 0x02,
    // arguments [argument, code]. crc = 0x02 ^ 0x40 ^ argument ^ code
    for (rate, code) in [
        (PollRate::Hz8000, 0x01u8),
        (PollRate::Hz4000, 0x02),
        (PollRate::Hz2000, 0x04),
        (PollRate::Hz1000, 0x08),
        (PollRate::Hz500, 0x10),
        (PollRate::Hz250, 0x20),
        (PollRate::Hz125, 0x40),
    ] {
        let crc_byte = xor(&[0x02, 0x40, 0x00, code]);
        let expected = expect_frame(0x1F, 0x02, 0x00, 0x40, &[0x00, code], crc_byte);
        let got = cmd::set_poll_rate_v2(rate, 0x00)
            .with_transaction_id(0x1F)
            .to_bytes();
        assert_eq!(got, expected, "{rate:?}");
    }
    // Spot-check two of those crcs as literals so the helper cannot drift.
    assert_eq!(xor(&[0x02, 0x40, 0x00, 0x01]), 0x43); // 8000 Hz
    assert_eq!(xor(&[0x02, 0x40, 0x00, 0x08]), 0x4A); // 1000 Hz

    // The `argument` byte is arguments[0] and is genuinely the caller's
    // (razerchromacommon.c:1157).
    let with_arg = cmd::set_poll_rate_v2(PollRate::Hz1000, 0x01).to_bytes();
    assert_eq!(with_arg[8], 0x01);
    assert_eq!(with_arg[9], 0x08);
}

#[test]
fn golden_get_poll_rate_v2() {
    // razerchromacommon.c:1138-1141. crc = 0x01 ^ 0xC0 = 0xC1
    let expected = expect_frame(0x1F, 0x01, 0x00, 0xC0, &[], 0xC1);
    assert_eq!(xor(&[0x01, 0xC0]), 0xC1, "hand crc");
    assert_eq!(
        cmd::get_poll_rate_v2().with_transaction_id(0x1F).to_bytes(),
        expected
    );
}

// ---------------------------------------------------------------------------
// The upstream bug itself
// ---------------------------------------------------------------------------

/// The malformed frame `razer_attr_write_device_mode()` emits
/// (`razerkbd_driver.c:4308`, `request.transaction_id.id = 0xFF`) differs from
/// the correct one in exactly one byte — index 1 — and both are checksum-valid.
///
/// That is the mechanism: the device has no way to reject the bad frame.
#[test]
fn the_upstream_bug_is_one_byte_and_the_checksum_cannot_see_it() {
    let base = cmd::set_device_mode(DeviceMode::Driver);

    let correct = base.with_transaction_id(0x1F).to_bytes(); // razerkbd_driver.c:546
    let upstream = base.with_transaction_id(0xFF).to_bytes(); // razerkbd_driver.c:4308

    let differing: Vec<usize> = (0..REPORT_LEN)
        .filter(|&i| correct[i] != upstream[i])
        .collect();
    assert_eq!(differing, vec![1], "the frames must differ only at byte 1");

    assert!(verify_crc(&correct));
    assert!(verify_crc(&upstream), "the bad frame is checksum-valid too");
    assert_eq!(correct[88], 0x05);
    assert_eq!(upstream[88], 0x05);
}

/// Byte 1 is outside the checksum range for *every* one of its 256 values, on a
/// realistic frame — not just for 0x1F vs 0xFF.
#[test]
fn transaction_id_is_outside_the_checksum_for_all_256_values() {
    let base = cmd::set_static_effect(Storage::VarStore, LedId::Backlight, Rgb::new(1, 2, 3));
    let baseline = base.to_bytes()[88];
    for tid in 0..=u8::MAX {
        let b = base.with_transaction_id(tid).to_bytes();
        assert_eq!(b[1], tid, "tid {tid:#04x} did not land at byte 1");
        assert_eq!(b[88], baseline, "tid {tid:#04x} moved the checksum");
    }
}

// ---------------------------------------------------------------------------
// Checksum, decode, and boundaries
// ---------------------------------------------------------------------------

/// `razer_calculate_crc` (`razercommon.c:110-122`) folds indices 2..=87, i.e.
/// exactly 86 bytes. Prove the count, not just the endpoints.
#[test]
fn checksum_covers_exactly_eighty_six_bytes() {
    let mut covered = 0usize;
    for i in 0..REPORT_LEN {
        let mut b = [0u8; REPORT_LEN];
        b[i] = 0xFF;
        if crc(&b) != 0x00 {
            covered += 1;
            assert!((2..=87).contains(&i), "byte {i} must not be covered");
        } else {
            assert!(!(2..=87).contains(&i), "byte {i} must be covered");
        }
    }
    assert_eq!(covered, 86);
}

/// Decoding then re-encoding a checksum-valid buffer must reproduce it exactly.
/// This is the inverse of acceptance criterion 7 and catches any field the
/// decoder drops.
#[test]
fn decode_then_encode_is_the_identity_on_a_valid_frame() {
    let mut b = [0u8; REPORT_LEN];
    for (i, x) in b.iter_mut().enumerate() {
        *x = (i as u8).wrapping_mul(31).wrapping_add(9);
    }
    b[5] = 0x50; // keep data_size legal (80)
    b[88] = crc(&b);
    assert!(verify_crc(&b));

    let round_tripped = RazerReport::from_bytes(&b).to_bytes();
    assert_eq!(round_tripped, b);
}

/// Every one of the 65536 `remaining_packets` values must survive the
/// big-endian encode/decode. A byte-swap bug would show up as soon as the two
/// halves differ.
#[test]
fn remaining_packets_survives_every_value_big_endian() {
    let mut r = cmd::get_device_mode();
    for v in 0..=u16::MAX {
        r.remaining_packets = v;
        let b = r.to_bytes();
        assert_eq!(b[2], (v >> 8) as u8, "high byte for {v:#06x}");
        assert_eq!(b[3], (v & 0xFF) as u8, "low byte for {v:#06x}");
        assert_eq!(RazerReport::from_bytes(&b).remaining_packets, v);
    }
}

#[test]
fn data_size_boundary_is_eighty_inclusive() {
    let req = cmd::get_device_mode();

    for size in [0u8, 1, 79, 80] {
        let mut a = req;
        a.data_size = size;
        let mut b = a;
        b.status = Status::Successful;
        assert_eq!(
            parse::check_response(&a, &b),
            Ok(()),
            "data_size {size} must be accepted"
        );
    }
    for size in [81u8, 90, 255] {
        let mut a = req;
        a.data_size = size;
        let mut b = a;
        b.status = Status::Successful;
        assert_eq!(
            parse::check_response(&a, &b),
            Err(ProtoError::DataSizeTooLarge { got: size }),
            "data_size {size} must be rejected"
        );
    }
}

/// `razer_send_payload` (`razerkbd_driver.c:437-441`) tests packets, then
/// class, then id. Lock the reporting order so the error names the first
/// disagreement, as the C log message implies.
#[test]
fn mismatch_reporting_order_is_packets_then_class_then_id() {
    let req = cmd::get_device_mode();
    let mut resp = req;
    resp.status = Status::Successful;
    resp.remaining_packets = 0x0001;
    resp.command_class = 0x0F;
    resp.command_id = 0x04;

    assert_eq!(
        parse::check_response(&req, &resp),
        Err(ProtoError::ResponseMismatch {
            field: "remaining_packets",
            expected: 0,
            got: 1,
        })
    );

    resp.remaining_packets = 0;
    assert_eq!(
        parse::check_response(&req, &resp),
        Err(ProtoError::ResponseMismatch {
            field: "command_class",
            expected: 0x00,
            got: 0x0F,
        })
    );

    resp.command_class = 0x00;
    assert_eq!(
        parse::check_response(&req, &resp),
        Err(ProtoError::ResponseMismatch {
            field: "command_id",
            expected: 0x84,
            got: 0x04,
        })
    );
}

/// A routing mismatch must be reported even when the device says "failed", so
/// the caller is not told the wrong story about which frame came back.
#[test]
fn routing_is_checked_before_status() {
    let req = cmd::get_device_mode();
    let mut resp = req;
    resp.status = Status::Failure;
    resp.command_class = 0x0F;
    assert!(matches!(
        parse::check_response(&req, &resp),
        Err(ProtoError::ResponseMismatch { .. })
    ));
}

/// Only 0x02 and 0x01 pass. Prove it across the whole byte space rather than a
/// hand-picked few — `razerkbd_driver.c:447-449`.
#[test]
fn exactly_two_status_bytes_are_accepted() {
    let req = cmd::get_device_mode();
    for v in 0..=u8::MAX {
        let mut resp = req;
        resp.status = Status::from_u8(v);
        let ok = parse::check_response(&req, &resp).is_ok();
        assert_eq!(
            ok,
            v == 0x01 || v == 0x02,
            "status {v:#04x} acceptance is wrong"
        );
    }
}

// ---------------------------------------------------------------------------
// Parsers: the index traps
// ---------------------------------------------------------------------------

/// One fixture, three readers, three different indices. If any reader drifts to
/// a neighbouring index this fails with a value that names the culprit.
#[test]
fn parsers_read_distinct_indices_on_one_fixture() {
    let mut resp = cmd::get_brightness(Storage::VarStore, LedId::Backlight);
    resp.status = Status::Successful;
    resp.arguments[0] = 0xA0;
    resp.arguments[1] = 0xA1;
    resp.arguments[2] = 0xA2;

    // brightness: arguments[2] (razerkbd_driver.c:4280-4282, non-Blade branch)
    assert_eq!(parse::brightness(&resp), Ok(0xA2));
    // firmware: arguments[0], arguments[1]
    assert_eq!(parse::firmware_version(&resp), Ok((0xA0, 0xA1)));
    // device mode: arguments[0], arguments[1]
    assert_eq!(parse::device_mode(&resp), Ok((0xA0, 0xA1)));
}

/// `razer_chroma_misc_get_polling_rate2` declares `data_size = 0x01` yet the
/// answer lives in `arguments[1]` (razerchromacommon.c:1125/1140). A reader
/// that clamped to `data_size` would silently return the wrong rate.
#[test]
fn v2_poll_rate_is_read_past_the_declared_data_size() {
    let mut resp = cmd::get_poll_rate_v2();
    assert_eq!(resp.data_size, 0x01, "upstream really does declare 1");
    resp.status = Status::Successful;
    resp.arguments[0] = 0x00;
    resp.arguments[1] = 0x40; // 125 Hz
    assert_eq!(parse::poll_rate_v2(&resp), Ok(PollRate::Hz125));
    assert_eq!(resp.args(), &[0x00], "args() clamps, the parser must not");
}

/// Both encodings share code bytes 0x01, 0x02 and 0x08 but mean different
/// rates. Reading a v2 answer with the legacy decoder must not accidentally
/// look plausible.
#[test]
fn the_two_poll_rate_encodings_disagree_on_shared_codes() {
    for (code, legacy, v2) in [
        (0x01u8, Some(PollRate::Hz1000), Some(PollRate::Hz8000)),
        (0x02, Some(PollRate::Hz500), Some(PollRate::Hz4000)),
        (0x08, Some(PollRate::Hz125), Some(PollRate::Hz1000)),
    ] {
        assert_eq!(
            PollRate::from_legacy_code(code),
            legacy,
            "legacy {code:#04x}"
        );
        assert_eq!(PollRate::from_v2_code(code), v2, "v2 {code:#04x}");
        assert_ne!(legacy, v2);
    }
}

/// Exhaustive: every byte value decodes to at most one rate, in each encoding,
/// and the encode/decode pair is consistent.
#[test]
fn poll_rate_code_tables_are_exhaustively_consistent() {
    let all = [
        PollRate::Hz125,
        PollRate::Hz250,
        PollRate::Hz500,
        PollRate::Hz1000,
        PollRate::Hz2000,
        PollRate::Hz4000,
        PollRate::Hz8000,
    ];
    for code in 0..=u8::MAX {
        match PollRate::from_legacy_code(code) {
            Some(r) => assert_eq!(r.legacy_code(), Some(code)),
            None => assert!(all.iter().all(|r| r.legacy_code() != Some(code))),
        }
        match PollRate::from_v2_code(code) {
            Some(r) => assert_eq!(r.v2_code(), code),
            None => assert!(all.iter().all(|r| r.v2_code() != code)),
        }
    }
}

/// `razer_attr_read_device_serial` (`razerkbd_driver.c:2165-2166`) copies
/// exactly 22 bytes and terminates. Anything after index 21 must never leak in,
/// and a NUL inside the field must terminate the string.
#[test]
fn serial_respects_the_twenty_two_byte_window() {
    let mut resp = cmd::get_serial();
    resp.status = Status::Successful;

    // Fill the whole argument array with junk, then write a short serial.
    resp.arguments = [b'Z'; ARGS_LEN];
    resp.arguments[..7].copy_from_slice(b"PM12345");
    resp.arguments[7] = 0x00;
    assert_eq!(parse::serial(&resp).expect("ascii"), "PM12345");

    // Exactly 22 printable bytes, junk from index 22 on: the junk must not appear.
    resp.arguments = [b'Z'; ARGS_LEN];
    resp.arguments[..22].copy_from_slice(b"XX1234567890123456789A");
    let s = parse::serial(&resp).expect("ascii");
    assert_eq!(s, "XX1234567890123456789A");
    assert_eq!(s.len(), 22);

    // The 21-char case from acceptance criterion 33.
    resp.arguments = [0u8; ARGS_LEN];
    resp.arguments[..22].copy_from_slice(b"XX1234567890123456789\0");
    let s = parse::serial(&resp).expect("ascii");
    assert_eq!(s, "XX1234567890123456789");
    assert_eq!(s.len(), 21);
    assert!(!s.contains('\0'));
    assert_eq!(s, s.trim_end());
}

#[test]
fn firmware_version_decodes_the_documented_pair() {
    let mut resp = cmd::get_firmware_version();
    resp.status = Status::Successful;
    resp.arguments[0] = 0x01;
    resp.arguments[1] = 0x0B;
    assert_eq!(parse::firmware_version(&resp), Ok((1, 11)));

    // Both bytes are raw u8, so 255.255 must survive rather than saturate.
    resp.arguments[0] = 0xFF;
    resp.arguments[1] = 0xFF;
    assert_eq!(parse::firmware_version(&resp), Ok((255, 255)));
}

// ---------------------------------------------------------------------------
// Structural guarantees
// ---------------------------------------------------------------------------

/// Acceptance criterion 27, restated from outside the crate: no constructor may
/// stamp byte 1. Asserted on the encoded frame, which is what reaches hardware.
#[test]
fn no_constructor_stamps_byte_one_on_the_wire() {
    let all = [
        cmd::set_device_mode(DeviceMode::Normal),
        cmd::set_device_mode(DeviceMode::Driver),
        cmd::get_device_mode(),
        cmd::get_firmware_version(),
        cmd::get_serial(),
        cmd::set_brightness(Storage::VarStore, LedId::Backlight, 0x80),
        cmd::set_brightness(Storage::NoStore, LedId::Zero, 0x00),
        cmd::get_brightness(Storage::VarStore, LedId::Backlight),
        cmd::set_static_effect(Storage::VarStore, LedId::Logo, Rgb::new(9, 9, 9)),
        cmd::set_effect_none(Storage::NoStore, LedId::ScrollWheel),
        cmd::set_poll_rate_legacy(PollRate::Hz1000).expect("legacy-encodable"),
        cmd::set_poll_rate_legacy(PollRate::Hz500).expect("legacy-encodable"),
        cmd::set_poll_rate_legacy(PollRate::Hz125).expect("legacy-encodable"),
        cmd::get_poll_rate_legacy(),
        cmd::set_poll_rate_v2(PollRate::Hz8000, 0x00),
        cmd::set_poll_rate_v2(PollRate::Hz125, 0x01),
        cmd::get_poll_rate_v2(),
    ];
    for r in all {
        let b = r.to_bytes();
        assert_eq!(
            b[1], 0x00,
            "class {:#04x} id {:#04x} stamped a transaction id",
            r.command_class, r.command_id
        );
        // And the rest of the header is the documented constant shape.
        assert_eq!(b[0], 0x00, "status");
        assert_eq!([b[2], b[3]], [0x00, 0x00], "remaining_packets");
        assert_eq!(b[4], 0x00, "protocol_type");
        assert_eq!(b[89], 0x00, "reserved");
        assert!(verify_crc(&b));
        assert!(b[5] as usize <= ARGS_LEN, "data_size must be <= 80");
    }
}

/// Rates the legacy command cannot express must be refused, not silently turned
/// into 500 Hz the way `razerchromacommon.c:1130-1133` does.
#[test]
fn unrepresentable_legacy_rates_are_refused_not_defaulted() {
    for rate in [
        PollRate::Hz250,
        PollRate::Hz2000,
        PollRate::Hz4000,
        PollRate::Hz8000,
    ] {
        match cmd::set_poll_rate_legacy(rate) {
            Err(ProtoError::Malformed(_)) => {}
            other => panic!("{rate:?} should be refused, got {other:?}"),
        }
    }
    // In particular it must NOT come back as the 500 Hz frame.
    let five_hundred = cmd::set_poll_rate_legacy(PollRate::Hz500)
        .expect("500 is encodable")
        .to_bytes();
    assert_eq!(five_hundred[8], 0x02);
    assert!(cmd::set_poll_rate_legacy(PollRate::Hz8000).is_err());
}

/// `Status::Unknown` can be built with a byte that already has a canonical
/// variant, and it then compares unequal to that variant. Decoding always
/// produces the canonical form, so this cannot arise from the wire — this test
/// pins the behaviour so a future change to `is_ok()` is a deliberate one.
#[test]
fn status_unknown_is_never_produced_by_decoding() {
    for v in 0..=u8::MAX {
        let s = Status::from_u8(v);
        assert_eq!(s.to_u8(), v);
        if (0x00..=0x05).contains(&v) {
            assert!(
                !matches!(s, Status::Unknown(_)),
                "{v:#04x} must decode to a named variant"
            );
        } else {
            assert_eq!(s, Status::Unknown(v));
        }
    }
    // The hand-built duplicate is unequal to the canonical variant, and is not
    // treated as success. Documented, not endorsed.
    assert_ne!(Status::Unknown(0x02), Status::Successful);
    assert!(!Status::Unknown(0x02).is_ok());
}

/// Every LED id and storage value in scope encodes to the `razercommon.h`
/// constant, checked through a real frame rather than an `as` cast.
#[test]
fn led_and_storage_constants_reach_the_wire() {
    for (led, byte) in [
        (LedId::Zero, 0x00u8),
        (LedId::ScrollWheel, 0x01),
        (LedId::Logo, 0x04),
        (LedId::Backlight, 0x05),
    ] {
        let b = cmd::set_brightness(Storage::VarStore, led, 0x40).to_bytes();
        assert_eq!(b[9], byte, "{led:?}");
    }
    for (storage, byte) in [(Storage::NoStore, 0x00u8), (Storage::VarStore, 0x01)] {
        let b = cmd::set_brightness(storage, LedId::Backlight, 0x40).to_bytes();
        assert_eq!(b[8], byte, "{storage:?}");
    }
}

/// `command_id` bit 7 is the direction bit
/// (`union command_id_union`, `razercommon.h:99-105`): 0x80 means device->host.
#[test]
fn direction_bit_matches_every_constructor_name() {
    let gets = [
        cmd::get_device_mode(),
        cmd::get_firmware_version(),
        cmd::get_serial(),
        cmd::get_brightness(Storage::VarStore, LedId::Backlight),
        cmd::get_poll_rate_legacy(),
        cmd::get_poll_rate_v2(),
    ];
    for r in gets {
        assert!(r.is_get(), "id {:#04x}", r.command_id);
        assert_eq!(r.command_id & 0x80, 0x80);
    }
    let sets = [
        cmd::set_device_mode(DeviceMode::Driver),
        cmd::set_brightness(Storage::VarStore, LedId::Backlight, 0x10),
        cmd::set_static_effect(Storage::VarStore, LedId::Backlight, Rgb::new(0, 0, 0)),
        cmd::set_effect_none(Storage::VarStore, LedId::Backlight),
        cmd::set_poll_rate_v2(PollRate::Hz1000, 0x00),
    ];
    for r in sets {
        assert!(!r.is_get(), "id {:#04x}", r.command_id);
    }
}

/// TRAP, pinned deliberately: a decoded report's `crc` field cannot be checked
/// through the struct, because `to_bytes()` always *recomputes* byte 88 and
/// ignores `self.crc`. `verify_crc(&report.to_bytes())` is therefore vacuously
/// true for any report, corrupt or not.
///
/// The only correct way to validate a device's checksum is to call
/// [`verify_crc`] on the **raw received buffer**, before `from_bytes`. The
/// transport crate must keep that buffer. `ProtoError::BadCrc` exists for it to
/// return; nothing in this crate constructs it.
#[test]
fn a_corrupt_checksum_survives_decode_and_reencode() {
    let good = cmd::set_device_mode(DeviceMode::Driver)
        .with_transaction_id(0x1F)
        .to_bytes();
    assert!(verify_crc(&good));

    let mut corrupt = good;
    corrupt[88] ^= 0xFF; // as if the device sent a bad checksum

    // Caught only on the raw buffer.
    assert!(!verify_crc(&corrupt), "raw-buffer check must catch it");
    assert_eq!(crc(&corrupt), 0x05);

    // Decoding preserves the bad byte in the field...
    let decoded = RazerReport::from_bytes(&corrupt);
    assert_eq!(decoded.crc, 0x05 ^ 0xFF);

    // ...but re-encoding silently repairs it, so this check is useless.
    assert!(
        verify_crc(&decoded.to_bytes()),
        "to_bytes() recomputes the crc: it can never report corruption"
    );
    assert_eq!(decoded.to_bytes(), good);
}
