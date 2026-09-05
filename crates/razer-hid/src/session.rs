// SPDX-License-Identifier: GPL-2.0-or-later
//! Binding a device table entry to a transport.
//!
//! # The one place byte 1 is written
//!
//! [`Session::send`] is the only function in the workspace that assigns the
//! report's `transaction_id`, and it takes the value from
//! `razer_devices::DeviceEntry::transaction_id`. No command constructor sets
//! it; no caller passes it.
//!
//! That is the whole architectural answer to the bug this project exists to
//! fix. `razer_attr_write_device_mode()` (`razerkbd_driver.c:4290-4315`) builds
//! its report with `razer_chroma_standard_set_device_mode()` and then, at line
//! 4308, overwrites the transaction id with a hardcoded `0xFF` — while the
//! driver's own per-device switch (`razerkbd_driver.c:536`, value at `:546`)
//! and every other BlackWidow V4 Pro code path use `0x1F`. The checksum covers
//! bytes 2..=87, so byte 1 is *outside* it: the malformed frame is perfectly
//! well-formed as far as the CRC is concerned, the device accepts it, and
//! nothing upstream notices. With the id sourced from one table there is
//! nowhere for a second, different value to come from.

use core::time::Duration;

use razer_devices::DeviceEntry;
use razer_devices::capabilities::Capabilities;
use razer_devices::table::PollRateKind;
use razer_proto::report::{LedId, Storage};
use razer_proto::{DeviceMode, PollRate, ProtoError, RazerReport, Rgb, cmd, parse};

use crate::error::HidError;
use crate::transport::{FeatureTransport, HIDRAW_BUF_LEN};

/// How many times a transaction is attempted before giving up.
///
/// `razer_send_payload()`, `razerkbd_driver.c:425`: `for (retry = 5; retry > 0; retry--)`.
const MAX_ATTEMPTS: usize = 5;

/// The pause between attempts.
///
/// `razer_send_payload()`: `fsleep(10000)` — 10 000 µs.
const RETRY_DELAY: Duration = Duration::from_millis(10);

/// Injectable time, so the device wait can be asserted without spending it.
pub trait Clock {
    /// Block for `d`.
    fn sleep(&mut self, d: Duration);
}

/// The real clock.
///
/// Its [`Clock`] impl holds the crate's only call into the standard library's
/// sleep — nothing else in `razer-hid` blocks the thread.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealClock;

impl Clock for RealClock {
    fn sleep(&mut self, d: Duration) {
        std::thread::sleep(d);
    }
}

/// A device table entry bound to a transport and a clock.
///
/// Borrows both mutably for its lifetime, so read a [`crate::MockTransport`]'s
/// recordings after the session has been dropped or gone out of scope.
pub struct Session<'a> {
    entry: &'static DeviceEntry,
    transport: &'a mut dyn FeatureTransport,
    clock: &'a mut dyn Clock,
    transaction_id: u8,
    last_attempts: u32,
    last_response_transaction_id: Option<u8>,
}

impl<'a> Session<'a> {
    /// Bind `entry` to `transport`, using `clock` for the mandatory post-write
    /// wait.
    ///
    /// The transaction id defaults to `entry.transaction_id`.
    pub fn new(
        entry: &'static DeviceEntry,
        transport: &'a mut dyn FeatureTransport,
        clock: &'a mut dyn Clock,
    ) -> Self {
        Self {
            entry,
            transport,
            clock,
            transaction_id: entry.transaction_id,
            last_attempts: 0,
            last_response_transaction_id: None,
        }
    }

    /// Attempts consumed by the most recent [`transact`](Session::transact).
    ///
    /// `1` means it succeeded first time. Anything higher means the device
    /// rejected a well-formed report and the C driver's retry policy kicked in
    /// — which is the closest thing a HID device has to a saturation signal, so
    /// `razerd` exports it.
    #[must_use]
    pub fn last_attempts(&self) -> u32 {
        self.last_attempts
    }

    /// Byte 1 of the last response received by
    /// [`transact`](Session::transact), if there has been one.
    ///
    /// The device echoes a transaction id back, and `check_response` ignores it
    /// — faithfully, because the C driver ignores it too. But for *this*
    /// project byte 1 is the entire subject, so whether the device echoes the id
    /// it was sent is the single most interesting value in a response. Recorded
    /// rather than enforced: a mismatch is a diagnostic signal for the Phase 3
    /// A/B, not grounds to reject a frame real hardware sent.
    ///
    /// Compare against [`transaction_id`](Session::transaction_id).
    #[must_use]
    pub fn last_response_transaction_id(&self) -> Option<u8> {
        self.last_response_transaction_id
    }

    /// Override the transaction id for this session.
    ///
    /// A diagnostic escape hatch and nothing else. It exists so the upstream
    /// fault can be reproduced deliberately, by hand, on real hardware —
    /// `0x1F` in one run and `0xFF` in the next, with everything else held
    /// constant. Nothing on a normal path calls it.
    ///
    /// On a BlackWidow V4 Pro, `0xFF` on a device-mode write is the frame that
    /// appears to reset the firmware and drop the keyboard off the USB bus.
    #[must_use]
    pub fn with_transaction_id_override(mut self, tid: u8) -> Self {
        self.transaction_id = tid;
        self
    }

    /// The device table entry this session is bound to.
    pub fn entry(&self) -> &'static DeviceEntry {
        self.entry
    }

    /// The transaction id this session stamps into byte 1.
    pub fn transaction_id(&self) -> u8 {
        self.transaction_id
    }

    /// Stamp the transaction id, encode, prefix the report id and issue one
    /// `HIDIOCSFEATURE`, then wait `entry.wait`. No read back.
    ///
    /// The wait is unconditional, matching `razer_send_control_msg()`
    /// (`razercommon.c:20-43`), which calls `fsleep(wait)` after every SET.
    ///
    /// The report is validated by [`parse::check_request`] **before** the ioctl.
    /// `RazerReport`'s fields are public, so a caller can hand this method a
    /// `data_size` that overruns the argument array or a non-zero
    /// `protocol_type`; on hardware where one malformed frame is the suspected
    /// cause of a firmware reset, discovering that from the response is too
    /// late.
    ///
    /// # Errors
    ///
    /// [`HidError::Proto`] if the report is malformed — returned before any
    /// bytes leave. [`HidError::Io`] if the ioctl fails.
    pub fn send(&mut self, request: &RazerReport) -> Result<(), HidError> {
        // Before the transaction id is even stamped: nothing malformed goes out.
        parse::check_request(request)?;

        let mut report = *request;
        // The overwrite is deliberate and total: whatever the caller left in
        // byte 1, the device table wins. See the module docs.
        report.transaction_id = self.transaction_id;

        let mut buf = [0u8; HIDRAW_BUF_LEN];
        // buf[0] is the hidraw report id, matching the low byte of the C
        // driver's wValue = 0x0300.
        buf[0] = 0x00;
        // `to_bytes` recomputes the checksum over bytes 2..=87.
        buf[1..].copy_from_slice(&report.to_bytes());

        self.transport.set_feature(&buf)?;
        self.clock.sleep(self.entry.wait);
        Ok(())
    }

    /// [`Session::send`], then one `HIDIOCGFEATURE`, decoded. No validation.
    ///
    /// # Errors
    ///
    /// [`HidError::Io`] if either ioctl fails; [`HidError::ShortRead`] if the
    /// device produced fewer than [`HIDRAW_BUF_LEN`] bytes.
    pub fn transact_raw(&mut self, request: &RazerReport) -> Result<RazerReport, HidError> {
        self.send(request)?;

        let mut buf = [0u8; HIDRAW_BUF_LEN];
        let got = self.transport.get_feature(&mut buf)?;
        if got < HIDRAW_BUF_LEN {
            return Err(HidError::ShortRead { got });
        }

        let mut report = [0u8; HIDRAW_BUF_LEN - 1];
        report.copy_from_slice(&buf[1..]);
        Ok(RazerReport::from_bytes(&report))
    }

    /// A full transaction with the C driver's retry policy.
    ///
    /// Up to five attempts, 10 ms apart, accepting a
    /// response only if `parse::check_response` does — which requires
    /// `remaining_packets`, `command_class` and `command_id` to echo the
    /// request, and the status to be `Successful` or `Busy`
    /// (`razerkbd_driver.c:440-449`).
    ///
    /// **Deviation from the C driver, deliberately:** upstream also retries a
    /// short/failed USB read. We propagate [`HidError::Io`] and
    /// [`HidError::ShortRead`] immediately instead. Hammering a device five
    /// times because the fd returned `EACCES` helps nobody, and a transport
    /// fault is not a protocol fault.
    ///
    /// **Note that a retry re-sends the SET.** Each attempt is a full
    /// [`transact_raw`](Session::transact_raw), so a rejected *write* — a
    /// `set_device_mode`, say — is re-issued up to five times, not merely
    /// re-read. That is faithful to `razer_send_payload()`, which does the same,
    /// and it is worth being explicit about because this project's own
    /// hypothesis is that a device-mode write can reset this firmware: on that
    /// hypothesis, five re-sends of a report the device just rejected amplify
    /// exactly the event under study. Kept faithful rather than clever, because
    /// diverging here would make the A/B in `docs/phase3-experiment.md` measure
    /// something other than what upstream does.
    ///
    /// # Errors
    ///
    /// [`HidError::RetriesExhausted`] carrying the last protocol error, or any
    /// transport error, immediately.
    pub fn transact(&mut self, request: &RazerReport) -> Result<RazerReport, HidError> {
        let mut last: Option<ProtoError> = None;

        for attempt in 0..MAX_ATTEMPTS {
            if attempt > 0 {
                self.clock.sleep(RETRY_DELAY);
            }
            self.last_attempts = u32::try_from(attempt).unwrap_or(u32::MAX).saturating_add(1);
            let response = self.transact_raw(request)?;
            self.last_response_transaction_id = Some(response.transaction_id);
            match parse::check_response(request, &response) {
                Ok(()) => return Ok(response),
                Err(e) => last = Some(e),
            }
        }

        Err(HidError::RetriesExhausted {
            last: last.unwrap_or(ProtoError::Malformed(
                "retry loop ended without a validated response",
            )),
        })
    }

    // -- typed operations ---------------------------------------------------

    /// Set the operating mode.
    ///
    /// **This is the command the C driver gets wrong.** Here the transaction id
    /// comes from the device table like every other command, so the V4 Pro gets
    /// `0x1F`.
    ///
    /// # Errors
    ///
    /// [`HidError::Unsupported`] if the device table says the model has no
    /// device-mode command — checked before anything is sent.
    pub fn set_device_mode(&mut self, mode: DeviceMode) -> Result<(), HidError> {
        self.require(Capabilities::DEVICE_MODE, "device mode")?;
        self.transact(&cmd::set_device_mode(mode))?;
        Ok(())
    }

    /// Read back the operating mode as `(mode, param)`.
    ///
    /// # Errors
    ///
    /// [`HidError::Unsupported`] if unsupported; transport or protocol errors
    /// otherwise.
    pub fn get_device_mode(&mut self) -> Result<(u8, u8), HidError> {
        self.require(Capabilities::DEVICE_MODE, "device mode")?;
        let response = self.transact(&cmd::get_device_mode())?;
        Ok(parse::device_mode(&response)?)
    }

    /// Read the firmware version as `(major, minor)`.
    ///
    /// # Errors
    ///
    /// [`HidError::Unsupported`] if unsupported; transport or protocol errors
    /// otherwise.
    pub fn firmware_version(&mut self) -> Result<(u8, u8), HidError> {
        self.require(Capabilities::FIRMWARE_VERSION, "firmware version")?;
        let response = self.transact(&cmd::get_firmware_version())?;
        Ok(parse::firmware_version(&response)?)
    }

    /// Read the device serial number.
    ///
    /// # Errors
    ///
    /// [`HidError::Unsupported`] if unsupported; transport or protocol errors
    /// otherwise.
    pub fn serial(&mut self) -> Result<String, HidError> {
        self.require(Capabilities::SERIAL, "serial")?;
        let response = self.transact(&cmd::get_serial())?;
        Ok(parse::serial(&response)?)
    }

    /// Set the backlight brightness.
    ///
    /// The LED id comes from the device table: `BACKLIGHT_LED` (0x05) for the
    /// keyboard, `ZERO_LED` (0x00) for the mouse.
    ///
    /// # Errors
    ///
    /// [`HidError::Unsupported`] if unsupported; transport or protocol errors
    /// otherwise.
    pub fn set_brightness(&mut self, brightness: u8) -> Result<(), HidError> {
        self.require(Capabilities::BRIGHTNESS, "brightness")?;
        let led = self.led()?;
        self.transact(&cmd::set_brightness(Storage::VarStore, led, brightness))?;
        Ok(())
    }

    /// Read back the backlight brightness.
    ///
    /// # Errors
    ///
    /// [`HidError::Unsupported`] if unsupported; transport or protocol errors
    /// otherwise.
    pub fn get_brightness(&mut self) -> Result<u8, HidError> {
        self.require(Capabilities::BRIGHTNESS, "brightness")?;
        let led = self.led()?;
        let response = self.transact(&cmd::get_brightness(Storage::VarStore, led))?;
        Ok(parse::brightness(&response)?)
    }

    /// Set the whole device to one static colour.
    ///
    /// # Errors
    ///
    /// [`HidError::Unsupported`] if unsupported; transport or protocol errors
    /// otherwise.
    pub fn set_static_effect(&mut self, rgb: Rgb) -> Result<(), HidError> {
        self.require(Capabilities::STATIC_EFFECT, "static effect")?;
        let led = self.led()?;
        self.transact(&cmd::set_static_effect(Storage::VarStore, led, rgb))?;
        Ok(())
    }

    /// Set the polling rate, using whichever command family the device speaks.
    ///
    /// Picking the wrong family is the most likely silent bug in this file:
    /// the BlackWidow V4 Pro wants v2 (`0x00`/`0x40`), the Basilisk V3 Pro
    /// legacy (`0x00`/`0x05`), and the encodings are not interchangeable. The
    /// choice comes from `entry.poll_rate_kind`, never from a guess.
    ///
    /// # Errors
    ///
    /// [`HidError::Unsupported`] if the device has no poll-rate command;
    /// [`HidError::Proto`] if the rate has no encoding in this device's family
    /// (legacy covers only 1000/500/125 Hz).
    pub fn set_poll_rate(&mut self, rate: PollRate) -> Result<(), HidError> {
        self.require(Capabilities::POLL_RATE, "poll rate")?;
        let request = match self.entry.poll_rate_kind {
            // `razer_attr_write_poll_rate`, V4 Pro block — argument 0x00.
            PollRateKind::V2 => cmd::set_poll_rate_v2(rate, 0x00),
            // `razer_attr_write_poll_rate`, razermouse_driver.c:2117-2140.
            PollRateKind::Legacy => cmd::set_poll_rate_legacy(rate)?,
        };
        self.transact(&request)?;
        Ok(())
    }

    /// Read back the polling rate.
    ///
    /// The two families put the code in different argument slots — v2 in
    /// `arguments[1]`, legacy in `arguments[0]` — which is handled by
    /// `razer_proto::parse`, not here.
    ///
    /// # Errors
    ///
    /// [`HidError::Unsupported`] if unsupported; transport or protocol errors
    /// otherwise.
    pub fn get_poll_rate(&mut self) -> Result<PollRate, HidError> {
        self.require(Capabilities::POLL_RATE, "poll rate")?;
        match self.entry.poll_rate_kind {
            PollRateKind::V2 => {
                let response = self.transact(&cmd::get_poll_rate_v2())?;
                Ok(parse::poll_rate_v2(&response)?)
            }
            PollRateKind::Legacy => {
                let response = self.transact(&cmd::get_poll_rate_legacy())?;
                Ok(parse::poll_rate_legacy(&response)?)
            }
        }
    }

    // -- internals ----------------------------------------------------------

    /// Capability guard. Fires before a single byte leaves.
    fn require(&self, cap: Capabilities, what: &'static str) -> Result<(), HidError> {
        if self.entry.capabilities.contains(cap) {
            Ok(())
        } else {
            Err(HidError::Unsupported {
                pid: self.entry.pid,
                what,
            })
        }
    }

    /// The device's default LED id, as the typed form the command constructors
    /// take.
    fn led(&self) -> Result<LedId, HidError> {
        match self.entry.default_led {
            0x00 => Ok(LedId::Zero),
            0x01 => Ok(LedId::ScrollWheel),
            0x04 => Ok(LedId::Logo),
            0x05 => Ok(LedId::Backlight),
            _ => Err(HidError::Unsupported {
                pid: self.entry.pid,
                what: "this LED id",
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::{MockClock, MockTransport};
    use razer_devices::table::DeviceKind;
    use razer_proto::report::Status;

    const BLACKWIDOW_V4_PRO: u16 = 0x028D;
    const BASILISK_WIRELESS: u16 = 0x00AB;
    const BASILISK_WIRED: u16 = 0x00AA;

    const KBD_WAIT: Duration = Duration::from_micros(600);
    const MOUSE_WAIT: Duration = Duration::from_micros(31_000);

    fn entry(pid: u16) -> &'static DeviceEntry {
        razer_devices::lookup(pid).unwrap_or_else(|| panic!("{pid:#06x} is in the table"))
    }

    /// An entry with nothing supported, for the capability-guard tests. Every
    /// real table entry supports everything, so the guard can only be exercised
    /// against a synthetic one.
    static NO_CAPABILITIES: DeviceEntry = DeviceEntry {
        pid: 0xDEAD,
        name: "synthetic capability-guard fixture",
        kind: DeviceKind::Keyboard,
        transaction_id: 0x1F,
        report_index: 0x03,
        response_index: 0x03,
        wait: KBD_WAIT,
        capabilities: Capabilities::NONE,
        default_led: 0x05,
        poll_rate_kind: PollRateKind::V2,
    };

    /// A well-formed success response echoing the request's routing fields —
    /// what `parse::check_response` demands.
    fn ok_response(request: &RazerReport, args: &[u8]) -> RazerReport {
        let mut r = RazerReport::new(request.command_class, request.command_id, request.data_size)
            .with_args(args);
        r.status = Status::Successful;
        r.remaining_packets = request.remaining_packets;
        r
    }

    /// Criterion 18, applied everywhere: every buffer that left the session
    /// must carry the checksum `razer-proto` computes over bytes 2..=87 of the
    /// report, and must carry the hidraw report-id prefix.
    fn assert_frames_well_formed(transport: &MockTransport) {
        for (i, buf) in transport.sent().iter().enumerate() {
            assert_eq!(buf.len(), HIDRAW_BUF_LEN, "buffer {i} is the wrong length");
            assert_eq!(buf[0], 0x00, "buffer {i}: missing report-id prefix");
            let mut report = [0u8; HIDRAW_BUF_LEN - 1];
            report.copy_from_slice(&buf[1..]);
            assert_eq!(
                buf[89],
                razer_proto::crc(&report),
                "buffer {i}: checksum does not cover bytes 2..=87 correctly"
            );
            assert_eq!(buf[90], 0x00, "buffer {i}: reserved byte must be zero");
        }
    }

    // -----------------------------------------------------------------------
    // Wire framing
    // -----------------------------------------------------------------------

    /// Criterion 13. The golden frame, byte for byte.
    #[test]
    fn device_mode_driver_encodes_to_the_golden_frame() {
        let mut transport = MockTransport::new();
        let mut clock = MockClock::new();
        {
            let mut session = Session::new(entry(BLACKWIDOW_V4_PRO), &mut transport, &mut clock);
            session
                .send(&cmd::set_device_mode(DeviceMode::Driver))
                .expect("the mock never fails");
        }

        // status 0x00, tid 0x1F, remaining_packets 0x0000, protocol 0x00,
        // data_size 0x02, class 0x00, id 0x04, args [0x03, 0x00], crc, reserved.
        // The checksum is XOR of bytes 2..=87: 0x02 ^ 0x04 ^ 0x03 = 0x05.
        let mut golden = [0u8; 90];
        golden[1] = 0x1F;
        golden[5] = 0x02;
        golden[7] = 0x04;
        golden[8] = 0x03;
        golden[88] = 0x05;

        let sent = transport.sent();
        assert_eq!(sent.len(), 1, "exactly one SET_REPORT");
        assert_eq!(sent[0][0], 0x00, "hidraw report id");
        assert_eq!(&sent[0][1..91], &golden[..], "the 90-byte report");
        assert_frames_well_formed(&transport);
    }

    /// **Criterion 14 — the headline test.**
    ///
    /// The upstream bug is `razerkbd_driver.c:4308`:
    /// `request.transaction_id.id = 0xFF;` inside
    /// `razer_attr_write_device_mode()`, where the driver's own per-device
    /// switch (`razerkbd_driver.c:536`, value at `:546`) says `0x1F` for this
    /// PID and every other V4 Pro path agrees.
    ///
    /// If this test goes red, the project has reintroduced the exact fault it
    /// exists to fix.
    #[test]
    fn device_mode_uses_0x1f_not_0xff_on_blackwidow_v4_pro() {
        let mut transport = MockTransport::new();
        let mut clock = MockClock::new();
        {
            let mut session = Session::new(entry(BLACKWIDOW_V4_PRO), &mut transport, &mut clock);
            session
                .send(&cmd::set_device_mode(DeviceMode::Driver))
                .expect("the mock never fails");
        }

        // buf[0] is the hidraw report id, so the report's byte 1 is buf[2].
        assert_eq!(
            transport.sent()[0][2],
            0x1F,
            "transaction id must be 0x1F for the BlackWidow V4 Pro"
        );
        assert_ne!(
            transport.sent()[0][2],
            0xFF,
            "0xFF is razerkbd_driver.c:4308's bug — the frame that drops the keyboard off the bus"
        );
    }

    /// Criterion 15.
    #[test]
    fn device_mode_uses_0x1f_on_both_basilisk_pids() {
        for pid in [BASILISK_WIRED, BASILISK_WIRELESS] {
            let mut transport = MockTransport::new();
            let mut clock = MockClock::new();
            {
                let mut session = Session::new(entry(pid), &mut transport, &mut clock);
                session
                    .send(&cmd::set_device_mode(DeviceMode::Driver))
                    .expect("the mock never fails");
            }
            assert_eq!(
                transport.sent()[0][2],
                0x1F,
                "{pid:#06x} must use transaction id 0x1F"
            );
            assert_frames_well_formed(&transport);
        }
    }

    /// Criterion 16. The session overwrites; it does not merge.
    #[test]
    fn a_preset_transaction_id_on_the_request_is_overwritten() {
        let mut transport = MockTransport::new();
        let mut clock = MockClock::new();
        {
            let mut session = Session::new(entry(BLACKWIDOW_V4_PRO), &mut transport, &mut clock);
            let poisoned = cmd::set_device_mode(DeviceMode::Driver).with_transaction_id(0xFF);
            session.send(&poisoned).expect("the mock never fails");
        }
        assert_eq!(
            transport.sent()[0][2],
            0x1F,
            "the device table must win over whatever the caller left in byte 1"
        );
    }

    /// Criterion 17. The A/B hatch, and the only route to 0xFF.
    #[test]
    fn transaction_id_override_emits_the_value_it_was_given() {
        let mut transport = MockTransport::new();
        let mut clock = MockClock::new();
        {
            let mut session = Session::new(entry(BLACKWIDOW_V4_PRO), &mut transport, &mut clock)
                .with_transaction_id_override(0xFF);
            assert_eq!(session.transaction_id(), 0xFF);
            assert_eq!(
                session.entry().transaction_id,
                0x1F,
                "the table is unchanged"
            );
            session
                .send(&cmd::set_device_mode(DeviceMode::Driver))
                .expect("the mock never fails");
        }
        assert_eq!(transport.sent()[0][2], 0xFF);
        // The checksum still validates — byte 1 is outside the covered range,
        // which is exactly why upstream's bug is invisible to the CRC.
        assert_frames_well_formed(&transport);
    }

    #[test]
    fn a_session_reports_the_entry_it_was_built_from() {
        let mut transport = MockTransport::new();
        let mut clock = MockClock::new();
        let session = Session::new(entry(BLACKWIDOW_V4_PRO), &mut transport, &mut clock);
        assert_eq!(session.entry().pid, BLACKWIDOW_V4_PRO);
        assert_eq!(session.transaction_id(), 0x1F);
    }

    // -----------------------------------------------------------------------
    // Timing and retries
    // -----------------------------------------------------------------------

    /// Criterion 19.
    #[test]
    fn one_send_on_the_keyboard_waits_600us_exactly_once() {
        let mut transport = MockTransport::new();
        let mut clock = MockClock::new();
        {
            let mut session = Session::new(entry(BLACKWIDOW_V4_PRO), &mut transport, &mut clock);
            session
                .send(&cmd::set_device_mode(DeviceMode::Driver))
                .expect("the mock never fails");
        }
        assert_eq!(clock.sleeps(), [KBD_WAIT]);
    }

    /// Criterion 20. ~52x the keyboard's wait; not a typo, not to be optimised.
    #[test]
    fn one_send_on_the_wireless_mouse_waits_31ms_exactly_once() {
        let mut transport = MockTransport::new();
        let mut clock = MockClock::new();
        {
            let mut session = Session::new(entry(BASILISK_WIRELESS), &mut transport, &mut clock);
            session
                .send(&cmd::set_device_mode(DeviceMode::Driver))
                .expect("the mock never fails");
        }
        assert_eq!(clock.sleeps(), [MOUSE_WAIT]);
        assert_eq!(clock.total(), MOUSE_WAIT);
    }

    /// Criterion 21.
    /// The Basilisk V3 Pro's dongle, plugged in with the mouse on its cable.
    /// It answers RAZER_CMD_TIMEOUT to everything: a definite answer from
    /// working hardware, not a fault. Callers must be able to tell it apart
    /// from a real failure so they can report it as a state.
    #[test]
    fn a_receiver_with_no_device_behind_it_is_recognisable() {
        let request = cmd::get_firmware_version();
        let mut transport = MockTransport::new();
        for _ in 0..MAX_ATTEMPTS {
            let mut idle = ok_response(&request, &[0x00, 0x00]);
            idle.status = Status::Timeout;
            transport.push_response(&idle);
        }
        let mut clock = MockClock::new();

        let err = {
            let mut session = Session::new(entry(BASILISK_WIRELESS), &mut transport, &mut clock);
            session
                .firmware_version()
                .expect_err("an idle receiver cannot report a firmware version")
        };

        assert!(
            err.is_receiver_idle(),
            "expected an idle receiver, got {err:?}"
        );
        assert_eq!(err.terminal_status(), Some(Status::Timeout));
        // The retry policy is unchanged: upstream retries all five times on
        // TIMEOUT before returning -ETIMEDOUT, and phase3-experiment.md
        // measures that loop.
        assert_eq!(transport.sent().len(), MAX_ATTEMPTS, "still five attempts");
        assert_frames_well_formed(&transport);
    }

    /// The distinction upstream draws with three different errnos. If these
    /// collapse together again, an unsupported command starts being reported
    /// as an absent mouse.
    #[test]
    fn other_terminal_statuses_are_not_mistaken_for_an_idle_receiver() {
        for status in [Status::Failure, Status::NotSupported] {
            let request = cmd::get_firmware_version();
            let mut transport = MockTransport::new();
            for _ in 0..MAX_ATTEMPTS {
                let mut bad = ok_response(&request, &[0x00, 0x00]);
                bad.status = status;
                transport.push_response(&bad);
            }
            let mut clock = MockClock::new();

            let err = {
                let mut session =
                    Session::new(entry(BASILISK_WIRELESS), &mut transport, &mut clock);
                session.firmware_version().expect_err("status is not ok")
            };

            assert_eq!(err.terminal_status(), Some(status));
            assert!(
                !err.is_receiver_idle(),
                "{status:?} must not read as an idle receiver"
            );
        }
    }

    #[test]
    fn a_permanently_mismatched_response_exhausts_five_attempts() {
        let request = cmd::get_firmware_version();
        let mut transport = MockTransport::new();
        for _ in 0..MAX_ATTEMPTS {
            // Right command id, wrong class — check_response must reject it.
            let mut wrong = ok_response(&request, &[0x01, 0x02]);
            wrong.command_class = 0x0F;
            transport.push_response(&wrong);
        }
        let mut clock = MockClock::new();

        let err = {
            let mut session = Session::new(entry(BLACKWIDOW_V4_PRO), &mut transport, &mut clock);
            session
                .transact(&request)
                .expect_err("a class mismatch is never accepted")
        };

        assert!(
            matches!(err, HidError::RetriesExhausted { .. }),
            "expected RetriesExhausted, got {err:?}"
        );
        assert_eq!(transport.sent().len(), 5, "five SET_REPORTs");
        let retry_sleeps = clock.sleeps().iter().filter(|d| **d == RETRY_DELAY).count();
        assert_eq!(retry_sleeps, 4, "four inter-attempt sleeps of 10 ms");
        let device_waits = clock.sleeps().iter().filter(|d| **d == KBD_WAIT).count();
        assert_eq!(device_waits, 5, "one 600 us device wait per SET");
        assert_frames_well_formed(&transport);
    }

    /// Criterion 22.
    #[test]
    fn a_transaction_that_succeeds_on_the_third_attempt_stops_there() {
        let request = cmd::get_firmware_version();
        let mut transport = MockTransport::new();
        for _ in 0..2 {
            let mut wrong = ok_response(&request, &[]);
            wrong.command_class = 0x0F;
            transport.push_response(&wrong);
        }
        transport.push_response(&ok_response(&request, &[0x01, 0x0B]));
        // A fourth response that must never be consumed.
        transport.push_response(&ok_response(&request, &[0xEE, 0xEE]));
        let mut clock = MockClock::new();

        let response = {
            let mut session = Session::new(entry(BLACKWIDOW_V4_PRO), &mut transport, &mut clock);
            session
                .transact(&request)
                .expect("the third response is good")
        };

        assert_eq!(response.arguments[0], 0x01);
        assert_eq!(response.arguments[1], 0x0B);
        assert_eq!(transport.sent().len(), 3, "exactly three SET_REPORTs");
        assert_eq!(transport.pending_responses(), 1, "the fourth was not read");
    }

    /// Criterion 23. `razerkbd_driver.c:447-449` — *"Some commands respond with
    /// 'busy' but succeed. Treat it as success."*
    #[test]
    fn a_busy_status_is_accepted_on_the_first_attempt() {
        let request = cmd::get_firmware_version();
        let mut transport = MockTransport::new();
        let mut busy = ok_response(&request, &[0x02, 0x03]);
        busy.status = Status::Busy;
        transport.push_response(&busy);
        let mut clock = MockClock::new();

        let response = {
            let mut session = Session::new(entry(BLACKWIDOW_V4_PRO), &mut transport, &mut clock);
            session.transact(&request).expect("busy counts as success")
        };

        assert_eq!(response.status, Status::Busy);
        assert_eq!(transport.sent().len(), 1, "no retry");
        assert!(
            !clock.sleeps().contains(&RETRY_DELAY),
            "no inter-attempt sleep"
        );
    }

    #[test]
    fn a_failure_status_is_retried_and_then_reported() {
        let request = cmd::get_firmware_version();
        let mut transport = MockTransport::new();
        for _ in 0..MAX_ATTEMPTS {
            let mut failed = ok_response(&request, &[]);
            failed.status = Status::Failure;
            transport.push_response(&failed);
        }
        let mut clock = MockClock::new();

        let err = {
            let mut session = Session::new(entry(BLACKWIDOW_V4_PRO), &mut transport, &mut clock);
            session
                .transact(&request)
                .expect_err("failure is not success")
        };
        assert!(matches!(
            err,
            HidError::RetriesExhausted {
                last: ProtoError::DeviceStatus(Status::Failure)
            }
        ));
    }

    // -----------------------------------------------------------------------
    // Capability gating
    // -----------------------------------------------------------------------

    /// Criterion 24. The guard fires before any bytes leave.
    #[test]
    fn an_unsupported_poll_rate_sends_nothing() {
        let mut transport = MockTransport::new();
        let mut clock = MockClock::new();
        let err = {
            let mut session = Session::new(&NO_CAPABILITIES, &mut transport, &mut clock);
            session
                .set_poll_rate(PollRate::Hz1000)
                .expect_err("the entry has no POLL_RATE capability")
        };
        assert!(
            matches!(
                err,
                HidError::Unsupported {
                    pid: 0xDEAD,
                    what: "poll rate"
                }
            ),
            "got {err:?}"
        );
        assert!(
            transport.sent().is_empty(),
            "a guard that fires after the write is not a guard"
        );
        assert!(clock.sleeps().is_empty(), "and it must not have waited");
    }

    #[test]
    fn every_capability_guard_sends_nothing() {
        let mut transport = MockTransport::new();
        let mut clock = MockClock::new();
        {
            let mut session = Session::new(&NO_CAPABILITIES, &mut transport, &mut clock);
            assert!(session.set_device_mode(DeviceMode::Driver).is_err());
            assert!(session.get_device_mode().is_err());
            assert!(session.firmware_version().is_err());
            assert!(session.serial().is_err());
            assert!(session.set_brightness(0x40).is_err());
            assert!(session.get_brightness().is_err());
            assert!(session.set_static_effect(Rgb::new(0xFF, 0, 0)).is_err());
            assert!(session.get_poll_rate().is_err());
        }
        assert!(transport.sent().is_empty());
    }

    /// Criterion 25. Picking the wrong poll-rate family is the most likely
    /// silent bug in this file, so assert both directions.
    #[test]
    fn poll_rate_family_follows_the_device_table() {
        // BlackWidow V4 Pro — v2: class 0x00, id 0x40, args [argument, code].
        let mut transport = MockTransport::new();
        let mut clock = MockClock::new();
        {
            let request = cmd::set_poll_rate_v2(PollRate::Hz1000, 0x00);
            transport.push_response(&ok_response(&request, &[]));
            let mut session = Session::new(entry(BLACKWIDOW_V4_PRO), &mut transport, &mut clock);
            session.set_poll_rate(PollRate::Hz1000).expect("v2 set");
        }
        let sent = transport.sent_reports();
        assert_eq!(sent[0].command_class, 0x00);
        assert_eq!(sent[0].command_id, 0x40, "V2 family");
        assert_eq!(sent[0].data_size, 0x02);
        assert_eq!(
            sent[0].args(),
            &[0x00, 0x08],
            "argument 0x00, 1000 Hz = 0x08"
        );
        assert_frames_well_formed(&transport);

        // Basilisk V3 Pro — legacy: class 0x00, id 0x05, args [code].
        let mut transport = MockTransport::new();
        let mut clock = MockClock::new();
        {
            let request = cmd::set_poll_rate_legacy(PollRate::Hz1000).expect("1000 Hz is legal");
            transport.push_response(&ok_response(&request, &[]));
            let mut session = Session::new(entry(BASILISK_WIRELESS), &mut transport, &mut clock);
            session.set_poll_rate(PollRate::Hz1000).expect("legacy set");
        }
        let sent = transport.sent_reports();
        assert_eq!(sent[0].command_class, 0x00);
        assert_eq!(sent[0].command_id, 0x05, "legacy family");
        assert_eq!(sent[0].data_size, 0x01);
        assert_eq!(sent[0].args(), &[0x01], "1000 Hz = 0x01 in legacy encoding");
        assert_frames_well_formed(&transport);
    }

    #[test]
    fn a_rate_the_legacy_family_cannot_express_is_refused() {
        let mut transport = MockTransport::new();
        let mut clock = MockClock::new();
        let err = {
            let mut session = Session::new(entry(BASILISK_WIRELESS), &mut transport, &mut clock);
            session
                .set_poll_rate(PollRate::Hz8000)
                .expect_err("legacy encodes only 1000/500/125 Hz")
        };
        assert!(matches!(err, HidError::Proto(_)), "got {err:?}");
        assert!(
            transport.sent().is_empty(),
            "refusing beats silently setting 500 Hz, which is what upstream does"
        );
    }

    /// Criterion 26. The two families read the code from different slots; the
    /// fixture makes those two bytes differ so a mix-up cannot pass.
    #[test]
    fn poll_rate_is_read_from_the_right_argument_slot() {
        // v2 puts the code in arguments[1]. 0x04 = 2000 Hz there;
        // arguments[0] holds 0x08, which would decode as 1000 Hz if read.
        let mut transport = MockTransport::new();
        let mut clock = MockClock::new();
        let rate = {
            let request = cmd::get_poll_rate_v2();
            transport.push_response(&ok_response(&request, &[0x08, 0x04]));
            let mut session = Session::new(entry(BLACKWIDOW_V4_PRO), &mut transport, &mut clock);
            session.get_poll_rate().expect("v2 get")
        };
        assert_eq!(rate, PollRate::Hz2000, "v2 must read arguments[1]");

        // Legacy puts the code in arguments[0]. 0x02 = 500 Hz there;
        // arguments[1] holds 0x08, which would decode as 125 Hz if read.
        let mut transport = MockTransport::new();
        let mut clock = MockClock::new();
        let rate = {
            let request = cmd::get_poll_rate_legacy();
            transport.push_response(&ok_response(&request, &[0x02, 0x08]));
            let mut session = Session::new(entry(BASILISK_WIRELESS), &mut transport, &mut clock);
            session.get_poll_rate().expect("legacy get")
        };
        assert_eq!(rate, PollRate::Hz500, "legacy must read arguments[0]");
    }

    /// Criterion 27.
    #[test]
    fn brightness_uses_the_devices_own_led_id() {
        for (pid, expected_led) in [(BLACKWIDOW_V4_PRO, 0x05u8), (BASILISK_WIRELESS, 0x00)] {
            let mut transport = MockTransport::new();
            let mut clock = MockClock::new();
            {
                let request = cmd::set_brightness(
                    Storage::VarStore,
                    if expected_led == 0x05 {
                        LedId::Backlight
                    } else {
                        LedId::Zero
                    },
                    0x80,
                );
                transport.push_response(&ok_response(&request, &[]));
                let mut session = Session::new(entry(pid), &mut transport, &mut clock);
                session.set_brightness(0x80).expect("set brightness");
            }
            let sent = transport.sent_reports();
            assert_eq!(sent[0].command_class, 0x0F);
            assert_eq!(sent[0].command_id, 0x04);
            assert_eq!(sent[0].arguments[0], 0x01, "VARSTORE");
            assert_eq!(
                sent[0].arguments[1], expected_led,
                "{pid:#06x}: wrong LED id"
            );
            assert_eq!(sent[0].arguments[2], 0x80);
            assert_frames_well_formed(&transport);
        }
    }

    /// Criterion 28.
    #[test]
    fn static_effect_uses_the_devices_own_led_id() {
        for (pid, expected_led) in [(BLACKWIDOW_V4_PRO, 0x05u8), (BASILISK_WIRELESS, 0x00)] {
            let mut transport = MockTransport::new();
            let mut clock = MockClock::new();
            {
                let request = cmd::set_static_effect(
                    Storage::VarStore,
                    if expected_led == 0x05 {
                        LedId::Backlight
                    } else {
                        LedId::Zero
                    },
                    Rgb::new(0xFF, 0x00, 0x00),
                );
                transport.push_response(&ok_response(&request, &[]));
                let mut session = Session::new(entry(pid), &mut transport, &mut clock);
                session
                    .set_static_effect(Rgb::new(0xFF, 0x00, 0x00))
                    .expect("set static effect");
            }
            let sent = transport.sent_reports();
            assert_eq!(sent[0].command_class, 0x0F);
            assert_eq!(sent[0].command_id, 0x02);
            assert_eq!(sent[0].data_size, 0x09);
            assert_eq!(
                sent[0].args(),
                &[0x01, expected_led, 0x01, 0x00, 0x00, 0x01, 0xFF, 0x00, 0x00],
                "{pid:#06x}: wrong static-effect payload"
            );
        }
    }

    // -----------------------------------------------------------------------
    // The remaining typed reads
    // -----------------------------------------------------------------------

    #[test]
    fn firmware_version_and_device_mode_and_serial_round_trip() {
        let mut transport = MockTransport::new();
        let mut clock = MockClock::new();

        let fw_request = cmd::get_firmware_version();
        transport.push_response(&ok_response(&fw_request, &[0x01, 0x0B]));

        let mode_request = cmd::get_device_mode();
        transport.push_response(&ok_response(&mode_request, &[0x03, 0x00]));

        let serial_request = cmd::get_serial();
        let mut serial_args = [0u8; 22];
        serial_args.copy_from_slice(b"TESTSERIAL00001\0\0\0\0\0\0\0");
        transport.push_response(&ok_response(&serial_request, &serial_args));

        let mut session = Session::new(entry(BLACKWIDOW_V4_PRO), &mut transport, &mut clock);
        assert_eq!(session.firmware_version().expect("firmware"), (0x01, 0x0B));
        assert_eq!(session.get_device_mode().expect("mode"), (0x03, 0x00));
        assert_eq!(session.serial().expect("serial"), "TESTSERIAL00001");
    }

    #[test]
    fn brightness_reads_back_from_argument_two() {
        let mut transport = MockTransport::new();
        let mut clock = MockClock::new();
        let brightness = {
            let request = cmd::get_brightness(Storage::VarStore, LedId::Backlight);
            transport.push_response(&ok_response(&request, &[0x01, 0x05, 0x7F]));
            let mut session = Session::new(entry(BLACKWIDOW_V4_PRO), &mut transport, &mut clock);
            session.get_brightness().expect("get brightness")
        };
        assert_eq!(brightness, 0x7F);
    }

    #[test]
    fn set_device_mode_sends_one_transaction_and_reads_it_back() {
        let request = cmd::set_device_mode(DeviceMode::Normal);
        let mut transport = MockTransport::new();
        transport.push_response(&ok_response(&request, &[0x00, 0x00]));
        let mut clock = MockClock::new();
        {
            let mut session = Session::new(entry(BLACKWIDOW_V4_PRO), &mut transport, &mut clock);
            session
                .set_device_mode(DeviceMode::Normal)
                .expect("normal mode");
        }
        let sent = transport.sent_reports();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].transaction_id, 0x1F);
        assert_eq!(sent[0].args(), &[0x00, 0x00]);
    }

    // -----------------------------------------------------------------------
    // Error handling
    // -----------------------------------------------------------------------

    /// Criterion 29.
    #[test]
    fn a_failing_set_feature_surfaces_the_errno() {
        let mut transport = MockTransport::new();
        transport.fail_next_set(libc::EACCES);
        let mut clock = MockClock::new();
        let err = {
            let mut session = Session::new(entry(BLACKWIDOW_V4_PRO), &mut transport, &mut clock);
            session
                .send(&cmd::set_device_mode(DeviceMode::Driver))
                .expect_err("the mock was told to fail")
        };
        assert!(
            matches!(
                err,
                HidError::Io {
                    op: "HIDIOCSFEATURE",
                    errno: 13
                }
            ),
            "got {err:?}"
        );
        assert!(
            clock.sleeps().is_empty(),
            "a failed write must not be followed by the device wait"
        );
    }

    /// Criterion 30. Nothing queued means nothing to read.
    #[test]
    fn a_short_feature_read_is_reported_as_such() {
        let mut transport = MockTransport::new();
        let mut clock = MockClock::new();
        let err = {
            let mut session = Session::new(entry(BLACKWIDOW_V4_PRO), &mut transport, &mut clock);
            session
                .transact_raw(&cmd::get_firmware_version())
                .expect_err("no response was queued")
        };
        assert!(matches!(err, HidError::ShortRead { got: 0 }), "got {err:?}");
    }

    #[test]
    fn a_transport_error_mid_transaction_is_not_retried() {
        let request = cmd::get_firmware_version();
        let mut transport = MockTransport::new();
        transport.fail_next_set(libc::EIO);
        let mut clock = MockClock::new();
        let err = {
            let mut session = Session::new(entry(BLACKWIDOW_V4_PRO), &mut transport, &mut clock);
            session.transact(&request).expect_err("the write failed")
        };
        assert!(matches!(err, HidError::Io { .. }), "got {err:?}");
        assert!(
            transport.sent().is_empty(),
            "no successful write should have been recorded"
        );
    }

    #[test]
    fn a_raw_response_buffer_is_decoded_verbatim() {
        let request = cmd::get_firmware_version();
        let mut raw = [0u8; HIDRAW_BUF_LEN];
        // Report bytes start at index 1.
        raw[1] = 0x02; // status: successful
        raw[2] = 0x1F; // transaction id
        raw[6] = 0x02; // data_size
        raw[7] = 0x00; // command_class
        raw[8] = 0x81; // command_id
        raw[9] = 0x01; // arguments[0]
        raw[10] = 0x0B; // arguments[1]
        let mut transport = MockTransport::new();
        transport.push_raw(raw);
        let mut clock = MockClock::new();

        let response = {
            let mut session = Session::new(entry(BLACKWIDOW_V4_PRO), &mut transport, &mut clock);
            session
                .transact_raw(&request)
                .expect("a full-length buffer")
        };
        assert_eq!(response.status, Status::Successful);
        assert_eq!(response.transaction_id, 0x1F);
        assert_eq!(response.command_id, 0x81);
        assert_eq!(response.arguments[0], 0x01);
        assert_eq!(response.arguments[1], 0x0B);
    }
}
