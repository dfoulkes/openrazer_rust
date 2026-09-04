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
use razer_proto::report::DeviceMode;
use razer_hid::{
    HardwareOptIn, HidrawDevice, RealClock, RealSysfs, Session, find_device, list_razer_nodes,
};

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
        "{:<10} {:<16} {:>6} {:>6}  {}",
        "NODE", "PATH", "PID", "IFACE", "SUPPORTED"
    );
    for n in &nodes {
        let supported = lookup(n.info.product_id)
            .map_or_else(|| "-".to_string(), |e| e.name.to_string());
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

/// Count `USB disconnect` lines in the current boot's kernel log.
///
/// Crude, but it is the same evidence the whole investigation rests on, and
/// having the tool report it removes a manual step from the experiment.
fn disconnect_count() -> Option<u64> {
    let out = std::process::Command::new("journalctl")
        .args(["-k", "-b", "--no-pager"])
        .output()
        .ok()?;
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| l.contains("USB disconnect"))
            .count() as u64,
    )
}

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
    let tid_override = args
        .iter()
        .position(|a| a == "--transaction-id")
        .and_then(|i| args.get(i + 1))
        .map(|v| {
            let v = v.trim_start_matches("0x").trim_start_matches("0X");
            u8::from_str_radix(v, 16)
        })
        .transpose()?;

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
    println!("  transaction id   0x{tid:02X}{}", if tid_override.is_some() {
        "   <-- OVERRIDDEN"
    } else {
        "   (from device table)"
    });
    if tid == 0xFF {
        println!("  *** 0xFF is the value the upstream C driver sends and the value");
        println!("  *** this project exists to avoid. Expect a firmware reset.");
    }
    println!("  wire             {}", hex(&request.to_bytes()));

    let before = disconnect_count();
    if let Some(b) = before {
        println!("  disconnects before: {b}");
    }

    let mut transport = HidrawDevice::open(&node.path, &opt_in)?;
    let mut clock = RealClock;
    let mut session = Session::new(entry, &mut transport, &mut clock);
    if let Some(t) = tid_override {
        session = session.with_transaction_id_override(t);
    }

    let started = std::time::Instant::now();
    let result = session.set_device_mode(mode);
    let elapsed = started.elapsed();
    println!("  attempts         {}", session.last_attempts());
    println!("  elapsed          {:.3} ms", elapsed.as_secs_f64() * 1000.0);
    match result {
        Ok(()) => println!("  result           OK"),
        Err(e) => println!("  result           ERROR: {e}"),
    }

    // Give the firmware time to misbehave if it is going to. The observed
    // delay between a bad write and the disconnect has been 4-73 seconds, so
    // this is a first look, not a clean bill of health.
    println!("\n  watching for 20 s ...");
    for i in 1..=20 {
        std::thread::sleep(std::time::Duration::from_secs(1));
        if let (Some(b), Some(a)) = (before, disconnect_count()) {
            if a > b {
                println!("  *** USB DISCONNECT after {i} s (count {b} -> {a})");
                println!("  *** replug the keyboard. This transaction id is NOT safe.");
                return Ok(());
            }
        }
    }
    if let (Some(b), Some(a)) = (before, disconnect_count()) {
        println!("  disconnects after:  {a} (was {b}) — no reset in 20 s");
    }
    println!("\n  NOTE: one trial is not the experiment. See docs/phase3-experiment.md.");
    Ok(())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
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

    for pid in pids {
        let entry: &'static DeviceEntry = lookup(pid).expect("checked above");
        let node = find_device(&sysfs, RAZER_VID, pid, entry.report_index)?;

        println!("\n{} (0x{:04X})", entry.name, entry.pid);
        println!("  node            {}", node.path.display());
        println!("  usb interface   {}", node.info.interface_number);
        println!("  transaction id  0x{:02X}", entry.transaction_id);

        let mut transport = match HidrawDevice::open(&node.path, &opt_in) {
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

        match session.firmware_version() {
            Ok((major, minor)) => println!("  firmware        v{major}.{minor}"),
            Err(e) => println!("  firmware        ERROR: {e}"),
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

    // Exiting 0 after reaching no device at all would be a lie to any script
    // wrapping this. Report success only if at least one device was opened.
    if opened == 0 && failed > 0 {
        return Err(format!("could not open any of the {failed} supported device(s)").into());
    }
    Ok(())
}
