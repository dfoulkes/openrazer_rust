# openrazer_rust

A userspace Rust driver for Razer devices, talking the Razer HID protocol directly over
`hidraw`, replacing the need for the C kernel module `razerkbd`.

## Why this exists

The upstream C driver (`openrazer`) has a real bug. In
`driver/razerkbd_driver.c`, the sysfs `device_mode` writer
`razer_attr_write_device_mode()` hardcodes the packet's transaction id to `0xFF`. But the
driver's own per-device helper `razer_set_device_mode()` lists the BlackWidow V4 Pro with
the correct transaction id, `0x1F`, and every other code path for that device uses `0x1F`
consistently. The malformed `0xFF` packet appears to reset the keyboard's firmware,
dropping it off the USB bus entirely — which is what causes the login-loop / dead-keyboard
fault this project exists to fix.

`razermouse_driver.c` gets this right (correct per-PID switch throughout), so the mouse is
unaffected; only the keyboard's sysfs `device_mode` path is broken.

This project reimplements the protocol in userspace Rust, with the transaction id sourced
from a single per-device table (`razer-devices`) rather than hardcoded at each call site,
so this class of bug can't recur.

## Supported devices

| Device | PID | Transaction ID | Notes |
|---|---|---|---|
| Razer BlackWidow V4 Pro | `0x028D` | `0x1F` | keyboard |
| Razer Basilisk V3 Pro | `0x00AB` (wireless) / `0x00AA` (wired) | `0x1F` | mouse |

No other device is in scope. The Razer Kiyo Pro webcam is explicitly excluded — it has no
Chroma support.

## Layout

- `crates/razer-proto` — the wire format (90-byte `razer_report`, CRC, command
  constructors, response parsing). No I/O.
- `crates/razer-devices` — the static device table (PID, transaction id, report index,
  capabilities). No I/O.
- `crates/razer-hid` — enumeration (`/sys/class/hidraw`) and transport
  (`HIDIOCSFEATURE`/`HIDIOCGFEATURE` over `/dev/hidraw*`), plus the `Session` type that
  binds a device table entry to a transport.
- `crates/razerd` — the future daemon binary. Currently a stub.

## Hardware access is opt-in only

**Nothing in this codebase touches a real device by default, and nothing in any test
does either.** Every code path capable of touching hardware — opening a `hidraw` node,
issuing an ioctl, reading `/sys`, sleeping for the device's post-write wait — is reachable
only by holding a `razer_hid::HardwareOptIn` token, and the only way to obtain one is the
explicitly-named `HardwareOptIn::i_understand_this_touches_real_hardware()`. All tests run
against in-memory fixtures (`razer_hid::mock`).

## Building

```sh
cargo build --workspace
cargo test --workspace
```

No hardware is required to build or test.

## Requirements

- Rust 1.94+ (edition 2024) — see `rust-toolchain.toml`
- Linux with `hidraw` (`CONFIG_HIDRAW`, standard on every mainstream distro)
- A systemd-based distro if you want the `uaccess` udev rule to work as-is; a group
  fallback is documented in the rule file for anything else

You do **not** need OpenRazer installed, and you do **not** need to blacklist or unbind
`razerkbd`. `hidraw` nodes coexist with whichever HID driver owns the interfaces, so this
driver works alongside the kernel module rather than fighting it. That is a deliberate
design choice: nothing here can ever leave a keyboard bound to no driver.

## Verifying on your own hardware

Before trusting any of this against your device, run the read-only validation harness:

```sh
sudo python3 tools/validate_hidraw.py --rung1
```

It does two things, in increasing order of risk:

- **Rung 0** — opens the `hidraw` node and issues `HIDIOCGRAWINFO`, which reads
  kernel-cached descriptor data. **No packet reaches the device.**
- **Rung 1** — sends the firmware-version and serial *read* commands, which are the same
  ones OpenRazer's daemon issues at every device init, and cross-checks the answers
  against what `razerkbd` reports via sysfs. It never sends `device_mode`.

If both values match, your transport works and the wire format is confirmed on your
hardware. If they do not, **stop** — something differs on your setup and the device table
below likely needs an entry for it.

The cross-check needs `razerkbd` loaded to have something to compare against. You do not
need it — see below — so if it is absent the harness reports the reads it got and says the
cross-check was skipped, rather than calling a working transport unproven.

## Adding your device

The supported table is deliberately small because every entry should be *verified*, not
guessed. To add yours:

1. Find your PID: `lsusb | grep 1532:`
2. Find the transaction id the C driver uses for it. In upstream OpenRazer's
   `driver/razerkbd_driver.c` (or `razermouse_driver.c`), locate your device in the
   `switch` inside `razer_set_device_mode()` and read the `transaction_id.id` for its
   case label. **Do not** copy the value from `razer_attr_write_device_mode()` — that
   function hardcodes `0xFF` for every keyboard, which is the bug this project exists to
   route around.
3. Find the report index from `razer_get_report_params()` for your PID.
4. Add the entry to `crates/razer-devices/src/table.rs`, citing the file and line each
   value came from — every existing entry does, and PRs that do not will be asked to.
5. Add the matching `(idProduct, bInterfaceNumber)` pair to `udev/60-razer-rust.rules`.
   The rules are deliberately per-device and per-interface — see the install section for
   why widening them to the whole vendor id is a bad idea.
6. Confirm with `tools/validate_hidraw.py`, then open a PR saying what you tested on.

## Installing

1. **udev rule** — grants access to the *one* `hidraw` node per supported device that this
   driver talks to. Upstream OpenRazer's rule sets permissions on the usb/input/hid
   subsystems but not on `/dev/hidraw*`, so without this the nodes are `root:root` mode
   `0600`.

   ```sh
   sudo cp udev/60-razer-rust.rules /etc/udev/rules.d/
   sudo udevadm control --reload-rules && sudo udevadm trigger
   ```

   It uses `TAG+="uaccess"`, which hands access to whoever is logged in at the local seat
   via ACLs — no group needs to exist and no user needs adding to one. A group-based
   fallback for headless, multi-seat or non-systemd setups is commented in the file.

   **The `60-` prefix is load-bearing.** systemd's `73-seat-late.rules` is what actually
   applies the ACL (`TAG=="uaccess" ... RUN{builtin}+="uaccess"`), so a rule that sets the
   tag at `99-` sets it twenty-six rules too late. The symptom is deeply misleading: the
   tag *is* present in `udevadm info` (`CURRENT_TAGS=:uaccess:seat:`) and the ACL is simply
   never written, so it looks like the rule never matched. Verify with `getfacl` and look
   for a **named** entry — `user:you:rw-`. A bare `user::rw-` is the owner (root), not you.

   The rules set permissions only. They have no `RUN+=`, bind and unbind nothing, and
   cannot affect whether your keyboard types.

   **They are scoped to the control interface on purpose.** Reading a `/dev/hidraw*` node
   returns the device's raw HID *input* reports, so read access to a keyboard's input
   interface is read access to your keystrokes — every one of them, including those typed
   into other applications, outside whatever isolation Wayland or X11 provides. That is a
   standing grant to every process running as you, whether or not `razerd` is running, and it
   is very likely why the kernel ships `/dev/hidraw*` root-only and why upstream OpenRazer
   does not hand it out.

   So each rule matches an exact `idProduct` *and* an exact `bInterfaceNumber` — the control
   interface, which carries no keystrokes. The BlackWidow V4 Pro's interface 0, which is the
   one that carries what you type, keeps its default root-only permissions. If you add a
   device to `crates/razer-devices/src/table.rs`, add its `(idProduct, bInterfaceNumber)`
   pair to the rule file too; do **not** widen the match back to `ATTRS{idVendor}=="1532"`
   alone.

2. **systemd user unit** — `systemd/razerd.service` is a template, not enabled by
   anything in this repo:

   ```sh
   mkdir -p ~/.config/systemd/user
   cp systemd/razerd.service ~/.config/systemd/user/
   systemctl --user daemon-reload
   systemctl --user start razerd.service
   ```

   It is a `Type=oneshot` that runs `razerd info`, because `razerd` is not a daemon yet:
   with no subcommand the binary prints usage and exits 1, so a `Type=simple` unit with
   `Restart=on-failure` would respawn for as long as the machine is up. It becomes a
   long-running unit when there is something to run. The unit ships with the usual
   hardening set; note that it sets `PrivateNetwork=yes`, which you must relax if you
   enable the Prometheus endpoint.

## Status

Phase 0: protocol, device table and transport, all covered by tests against in-memory
fixtures. `razerd` is still a stub — there is no daemon to run yet.

The byte vectors in `crates/razer-proto/tests/fixtures/golden_vectors.txt` were generated
from a reference implementation validated against a real BlackWidow V4 Pro, and two of
them were confirmed against live firmware responses. `docs/phase3-experiment.md`
pre-registers the controlled A/B intended to prove the `0xFF` transaction id is what
resets the device.

## Licence

GPL-2.0-or-later, matching upstream OpenRazer — see `LICENSE`. This is a clean-room-ish
reimplementation informed by OpenRazer's GPL-licensed C source.
