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

use std::process::ExitCode;

use razer_devices::{DeviceEntry, lookup};
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

Both subcommands are read-only. Nothing in this binary writes to a device.

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
