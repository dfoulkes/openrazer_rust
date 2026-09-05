// SPDX-License-Identifier: GPL-2.0-or-later
//!
//! `razerd` — command-line front end for the userspace Razer driver.
//!
//! Phase 1 is deliberately **read-only**. The only subcommands here issue
//! GET-class commands (firmware version, serial, device mode), which are the
//! same reads OpenRazer's daemon performs at every device init. Nothing in this
//! binary writes to a device.
//!
//! Writes arrive in a later phase, behind their own subcommand and their own
//! confirmation, because on a BlackWidow V4 Pro a bad write can drop the
//! keyboard off the USB bus.

#[allow(dead_code)] // wired up in the observability phase; tests exercise it now
mod metrics;

use std::process::ExitCode;

use razer_devices::{DeviceEntry, lookup};
use razer_hid::{
    HardwareOptIn, HidrawDevice, RealClock, RealSysfs, Session, find_device, list_razer_nodes,
};
use razer_proto::report::DeviceMode;

const RAZER_VID: u16 = 0x1532;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("info") => match info() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
        Some("list") => match list() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
        Some("set-device-mode") => match set_device_mode(&args[1..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
        Some("--version" | "-V") => {
            println!("razerd {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        _ => {
            usage();
            ExitCode::FAILURE
        }
    }
}

fn usage() {
    eprintln!(
        "razerd {} — userspace Razer driver

USAGE:
    razerd list      Enumerate Razer hidraw nodes (reads sysfs only, no device I/O)
    razerd info      Read firmware version, serial and device mode from each
                     supported device (READ-ONLY: GET commands only)

    razerd set-device-mode <normal|driver> --yes-i-am-at-the-desk
                     WRITES to the device. Sets the operating mode.
                     Optional: --transaction-id 0xNN to override the device
                     table's value. Overriding it is how the upstream defect is
                     reproduced deliberately; do not use it casually.

`list` and `info` are read-only. `set-device-mode` is not.

If you get a permission error, install the udev rule:
    sudo cp udev/60-razer-rust.rules /etc/udev/rules.d/
    sudo udevadm control --reload-rules && sudo udevadm trigger",
        env!("CARGO_PKG_VERSION")
    );
}

/// Enumerate without opening any device node. Reads sysfs metadata only —
/// but still requires the opt-in token, because `RealSysfs` is gated too.
fn list() -> Result<(), Box<dyn std::error::Error>> {
    let opt_in = HardwareOptIn::i_understand_this_touches_real_hardware();
    let sysfs = RealSysfs::new(&opt_in);
    let nodes = list_razer_nodes(&sysfs)?;
    if nodes.is_empty() {
        println!("No Razer hidraw nodes found.");
        return Ok(());
    }
    println!(
        "{:<10} {:<16} {:>6} {:>6}  SUPPORTED",
        "NODE", "PATH", "PID", "IFACE"
    );
    for n in &nodes {
        let supported =
            lookup(n.info.product_id).map_or_else(|| "-".to_string(), |e| e.name.to_string());
        println!(
            "{:<10} {:<16} 0x{:04X} {:>6}  {}",
            n.name,
            n.path.display(),
            n.info.product_id,
            n.info.interface_number,
            supported
        );
    }
    Ok(())
}

/// Absolute paths to try for `journalctl`, in order.
///
/// Absolute, never a bare name: this binary's own usage text tells people to
/// reach for `sudo` when they hit a permission error, and a `$PATH` lookup in a
/// process running as root is a lookup an attacker gets a say in. On any
/// systemd machine the first entry is the one that exists.
const JOURNALCTL: [&str; 2] = ["/usr/bin/journalctl", "/bin/journalctl"];

/// Count `USB disconnect` lines in the current boot's kernel log.
///
/// Crude, but it is the same evidence the whole investigation rests on, and
/// having the tool report it removes a manual step from the experiment.
fn disconnect_count() -> Option<u64> {
    for exe in JOURNALCTL {
        let Ok(out) = std::process::Command::new(exe)
            .args(["-k", "-b", "--no-pager"])
            .output()
        else {
            continue;
        };
        return Some(
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter(|l| l.contains("USB disconnect"))
                .count() as u64,
        );
    }
    None
}

/// How long to watch for a firmware reset after a device-mode write.
///
/// The observed delay between a bad write and the `USB disconnect` has ranged
/// from 4 s to 73 s (`docs/phase3-experiment.md`), so anything shorter than the
/// top of that range reports "no reset" for cases it simply did not wait for.
const WATCH_SECONDS: u32 = 90;

/// Set the operating mode on the keyboard. THIS WRITES TO THE DEVICE.
///
/// On a BlackWidow V4 Pro this is the exact command that, sent by the C driver
/// with transaction id `0xFF`, resets the firmware and drops the keyboard off
/// the USB bus. Here it goes out with the device table's `0x1F` unless
/// deliberately overridden.
fn set_device_mode(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mode = match args.first().map(String::as_str) {
        Some("normal") => DeviceMode::Normal,
        Some("driver") => DeviceMode::Driver,
        _ => return Err("expected `normal` or `driver`".into()),
    };
    if !args.iter().any(|a| a == "--yes-i-am-at-the-desk") {
        return Err(
            "refusing to write without --yes-i-am-at-the-desk.\n             This command can reset the keyboard's firmware and drop it off the USB\n             bus. Recovery is a physical replug, so a human needs to be present."
                .into(),
        );
    }
    let tid_override = match args.iter().position(|a| a == "--transaction-id") {
        None => None,
        Some(i) => {
            let raw = args
                .get(i + 1)
                .ok_or("--transaction-id needs a value, e.g. --transaction-id 1F")?;
            // A flag here means the value was forgotten and the next argument
            // got eaten. Saying so beats "invalid digit found in string" —
            // especially when the argument eaten is --yes-i-am-at-the-desk.
            if raw.starts_with('-') {
                return Err(format!(
                    "--transaction-id needs a hex value, but got the flag `{raw}`"
                )
                .into());
            }
            // strip_prefix once, not trim_start_matches: the latter strips
            // repeats, so `0x0x0xFF` would parse happily as 0xFF.
            let digits = raw
                .strip_prefix("0x")
                .or_else(|| raw.strip_prefix("0X"))
                .unwrap_or(raw);
            if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(format!("`{raw}` is not a hex byte, e.g. 1F or 0x1F").into());
            }
            Some(u8::from_str_radix(digits, 16)?)
        }
    };

    let opt_in = HardwareOptIn::i_understand_this_touches_real_hardware();
    let sysfs = RealSysfs::new(&opt_in);

    // Keyboards only — this is where the defect lives, and there is no reason
    // to point it at a mouse.
    let pid: u16 = 0x028D;
    let entry: &'static DeviceEntry = lookup(pid).ok_or("BlackWidow V4 Pro not in device table")?;
    let node = find_device(&sysfs, RAZER_VID, pid, entry.report_index)?;

    let tid = tid_override.unwrap_or(entry.transaction_id);
    let request = razer_proto::cmd::set_device_mode(mode).with_transaction_id(tid);

    println!("{} ({:#06X})", entry.name, entry.pid);
    println!("  node             {}", node.path.display());
    println!("  mode             {mode:?} (0x{:02X})", mode.to_u8());
    println!(
        "  transaction id   0x{tid:02X}{}",
        if tid_override.is_some() {
            "   <-- OVERRIDDEN"
        } else {
            "   (from device table)"
        }
    );
    if tid == 0xFF {
        println!("  *** 0xFF is the value the upstream C driver sends and the value");
        println!("  *** this project exists to avoid. Expect a firmware reset.");
    }
    println!("  wire             {}", hex(&request.to_bytes()));

    let before = disconnect_count();
    if let Some(b) = before {
        println!("  disconnects before: {b}");
    }

    // open_expecting, not open: this path writes to the device, and a
    // re-enumeration between find_device and here can leave a different device
    // on the same /dev/hidrawN. Re-enumeration is the exact fault under study.
    let mut transport = HidrawDevice::open_expecting(&node.path, RAZER_VID, pid, &opt_in)?;
    let mut clock = RealClock;
    let mut session = Session::new(entry, &mut transport, &mut clock);
    if let Some(t) = tid_override {
        session = session.with_transaction_id_override(t);
    }

    let started = std::time::Instant::now();
    let result = session.set_device_mode(mode);
    let elapsed = started.elapsed();
    println!("  attempts         {}", session.last_attempts());
    println!(
        "  elapsed          {:.3} ms",
        elapsed.as_secs_f64() * 1000.0
    );
    if let Some(echoed) = session.last_response_transaction_id() {
        let note = if echoed == tid { "" } else { "   <-- DIFFERS" };
        println!("  echoed txn id    0x{echoed:02X}{note}");
    }
    match result {
        Ok(()) => println!("  result           OK"),
        Err(e) => println!("  result           ERROR: {e}"),
    }

    // Give the firmware time to misbehave if it is going to. The observed delay
    // between a bad write and the disconnect spans 4-73 s, so the window has to
    // clear the top of that range or a negative result means nothing.
    println!("\n  watching for {WATCH_SECONDS} s ...");
    for i in 1..=WATCH_SECONDS {
        std::thread::sleep(std::time::Duration::from_secs(1));
        if let (Some(b), Some(a)) = (before, disconnect_count())
            && a > b
        {
            println!("  *** USB DISCONNECT after {i} s (count {b} -> {a})");
            println!("  *** replug the keyboard. This transaction id is NOT safe.");
            return Ok(());
        }
    }
    if let (Some(b), Some(a)) = (before, disconnect_count()) {
        println!("  disconnects after:  {a} (was {b}) — no reset within {WATCH_SECONDS} s");
        println!("  (the observed range is 4-73 s, so this window clears it — but see below)");
    }
    println!("\n  NOTE: one trial is not the experiment. See docs/phase3-experiment.md.");
    Ok(())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// What `info` reports for one device, decided from its first probe.
///
/// Split out from [`info`] so the decision is testable: `info` itself opens
/// real hidraw nodes and prints, so the branch that matters — telling an idle
/// receiver apart from a broken device — could not otherwise be covered.
#[derive(Debug, PartialEq, Eq)]
enum Probe {
    /// The device answered with its firmware version.
    Firmware(u8, u8),
    /// A wireless receiver that is present but has no device behind it.
    ///
    /// Normal whenever the mouse is on its cable: the dongle stays enumerated
    /// and answers `RAZER_CMD_TIMEOUT` to everything. A state, not an error.
    ReceiverIdle,
    /// Anything else, rendered for display.
    Failed(String),
}

impl Probe {
    fn classify(result: Result<(u8, u8), razer_hid::HidError>) -> Self {
        match result {
            Ok((major, minor)) => Self::Firmware(major, minor),
            Err(e) if e.is_receiver_idle() => Self::ReceiverIdle,
            Err(e) => Self::Failed(e.to_string()),
        }
    }
}

/// Read-only device interrogation.
fn info() -> Result<(), Box<dyn std::error::Error>> {
    let opt_in = HardwareOptIn::i_understand_this_touches_real_hardware();
    let sysfs = RealSysfs::new(&opt_in);
    let nodes = list_razer_nodes(&sysfs)?;

    // Distinct PIDs we have a table entry for, in enumeration order.
    let mut pids: Vec<u16> = Vec::new();
    for n in &nodes {
        if lookup(n.info.product_id).is_some() && !pids.contains(&n.info.product_id) {
            pids.push(n.info.product_id);
        }
    }

    if pids.is_empty() {
        println!("No supported Razer devices found.");
        println!("(`razerd list` shows every Razer hidraw node, supported or not.)");
        return Ok(());
    }

    let mut opened = 0usize;
    let mut failed = 0usize;
    let mut idle = 0usize;

    for pid in pids {
        let entry: &'static DeviceEntry = lookup(pid).expect("checked above");

        println!("\n{} (0x{:04X})", entry.name, entry.pid);

        // A device that vanished between the listing above and this lookup is
        // one device's problem, not the run's. Handled the same way as a failed
        // open two lines down, rather than abandoning every remaining device.
        let node = match find_device(&sysfs, RAZER_VID, pid, entry.report_index) {
            Ok(n) => n,
            Err(e) => {
                println!("  ERROR locating the control interface: {e}");
                println!("  (unplugged mid-run? try again)");
                failed += 1;
                continue;
            }
        };

        println!("  node            {}", node.path.display());
        println!("  usb interface   {}", node.info.interface_number);
        println!("  transaction id  0x{:02X}", entry.transaction_id);

        let mut transport = match HidrawDevice::open_expecting(&node.path, RAZER_VID, pid, &opt_in)
        {
            Ok(t) => t,
            Err(e) => {
                println!("  ERROR opening {}: {e}", node.path.display());
                println!("  (permissions? install udev/60-razer-rust.rules — see `razerd` usage)");
                failed += 1;
                continue;
            }
        };
        opened += 1;
        let mut clock = RealClock;
        let mut session = Session::new(entry, &mut transport, &mut clock);

        // Probe once before doing the full set. A wireless receiver that is
        // plugged in with no mouse behind it — the normal state whenever the
        // mouse is on its cable, because the dongle stays enumerated — answers
        // RAZER_CMD_TIMEOUT to everything. That is a definite answer from a
        // working dongle, not a fault, so report it as a state and move on.
        // Carrying on would burn ten more retries to print the same misleading
        // ERROR twice again, and list a working mouse as broken two entries
        // below its own wired self.
        match Probe::classify(session.firmware_version()) {
            Probe::Firmware(major, minor) => println!("  firmware        v{major}.{minor}"),
            Probe::ReceiverIdle => {
                println!("  state           no device connected (receiver idle)");
                println!("                  dongle answered 0x04 TIMEOUT; the mouse is");
                println!("                  elsewhere — on its cable, or powered off");
                idle += 1;
                continue;
            }
            Probe::Failed(msg) => println!("  firmware        ERROR: {msg}"),
        }
        match session.serial() {
            Ok(s) => println!("  serial          {s}"),
            Err(e) => println!("  serial          ERROR: {e}"),
        }
        // GET device mode — command id 0x84, a read. Does not change the device.
        match session.get_device_mode() {
            Ok((mode, param)) => {
                let label = match mode {
                    0x00 => "normal (device)",
                    0x03 => "driver",
                    _ => "unknown",
                };
                println!("  device mode     0x{mode:02X} {label} (param 0x{param:02X})");
            }
            Err(e) => println!("  device mode     ERROR: {e}"),
        }
    }

    println!("\nAll commands above were GET-class reads. Nothing was written.");
    if idle > 0 {
        println!("{idle} receiver(s) had no device connected; that is a state, not an error.");
    }

    // Exiting 0 after reaching no device at all would be a lie to any script
    // wrapping this. Report success only if at least one device was opened.
    //
    // An idle receiver counts as opened: the dongle is present and answering,
    // and saying otherwise would make `razerd info` fail merely because the
    // mouse is on its cable.
    if opened == 0 && failed > 0 {
        return Err(format!("could not open any of the {failed} supported device(s)").into());
    }
    Ok(())
}

#[cfg(test)]
mod probe_tests {
    use super::Probe;
    use razer_hid::HidError;
    use razer_proto::{ProtoError, report::Status};

    #[test]
    fn a_firmware_read_reports_its_version() {
        assert_eq!(Probe::classify(Ok((1, 4))), Probe::Firmware(1, 4));
    }

    /// The case this whole change exists for: the Basilisk's dongle with the
    /// mouse on its cable must read as a state, never as a failure.
    #[test]
    fn a_timeout_from_an_idle_receiver_is_a_state_not_a_failure() {
        let err = HidError::RetriesExhausted {
            last: ProtoError::DeviceStatus(Status::Timeout),
        };
        assert_eq!(Probe::classify(Err(err)), Probe::ReceiverIdle);
    }

    /// Upstream gives FAILURE, NOT_SUPPORTED and TIMEOUT three different
    /// errnos. If these collapse, an unsupported command starts being
    /// reported to the user as an absent mouse.
    #[test]
    fn other_terminal_statuses_still_report_as_failures() {
        for status in [Status::Failure, Status::NotSupported] {
            let err = HidError::RetriesExhausted {
                last: ProtoError::DeviceStatus(status),
            };
            assert!(
                matches!(Probe::classify(Err(err)), Probe::Failed(_)),
                "{status:?} must report as a failure"
            );
        }
    }

    /// A permissions problem is not an absent mouse. This is the regression
    /// that would hide a broken udev rule behind a reassuring message.
    #[test]
    fn a_permission_error_is_a_failure_not_an_idle_receiver() {
        let err = HidError::Io {
            op: "HIDIOCSFEATURE",
            errno: 13,
        };
        assert!(matches!(Probe::classify(Err(err)), Probe::Failed(_)));
    }
}
