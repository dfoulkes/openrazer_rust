# Phase 3 — controlled A/B: does transaction id 0xFF reset the BlackWidow V4 Pro?

**Status:** pre-registered. Predictions below are written BEFORE any arm is run, so a result
cannot be rationalised after the fact.

**Device:** Razer BlackWidow V4 Pro, `1532:028d`, serial `XX0000000000000`, on `arch-dan`.
**Runs:** by hand, in the main session, with Dan present. No agent performs any arm of this.

---

## Hypothesis

`razerkbd_driver.c:4308` hardcodes `transaction_id.id = 0xFF` in the sysfs `device_mode` writer.
The V4 Pro requires `0x1F` (`razerkbd_driver.c:536/546`, and every other V4 Pro path in the file).
Sending the device-mode report with `0xFF` causes a firmware reset that presents to the kernel as
`usb ...: USB disconnect`.

### Pre-registered predictions

| Arm | Report | Transaction ID | Predicted disconnects in 20 trials |
|---|---|---|---|
| **A — baseline** | none (idle) | — | **0** |
| **B — the bug** | `device_mode 0x03 0x00` | `0xFF` | **≥ 1**, and materially more than arm A |
| **C — the fix** | `device_mode 0x03 0x00` | `0x1F` | **0** |
| **D — poll rate** | `poll_rate 2000` | `0x1F` | **0** (kills the competing hypothesis) |

A result of "B produces disconnects, C does not" is the finding. Anything else — including B
producing none — kills the hypothesis and we say so plainly rather than re-running until it agrees.

**Why 20 trials and not one.** The fault is intermittent. On 2026-08-03 it fired three times in one
boot then survived a fourth write; on 2026-09-04 it fired at +73 s, then at +4 s, then survived.
A single passing trial of arm C would prove nothing. 20 trials at 30 s spacing per arm, ~10 min
per arm.

---

## Preconditions — every one of these before arm B

1. **Dan is physically at the desk.** Arm B is *designed* to drop the keyboard. Recovery is a
   physical replug, so it needs a human hand on the cable.
2. **`openrazer-daemon` stopped** — `systemctl --user stop openrazer-daemon.service`.
   Otherwise it piles its own driver-mode writes on top and the arms are not isolated.
   Confirm with `systemctl --user is-active openrazer-daemon` → `inactive`.
3. **`polychromatic-tray-applet` killed** — it will restart the daemon over DBus.
4. **`razer-leds-boot.service` masked for the duration** — `/etc/udev/rules.d/99-razer-leds.rules`
   restarts it on every BlackWidow `add` event, i.e. on every re-enumeration during arm B.
5. **A second terminal running `sudo dmesg -w --time-format=iso`**, captured to a file. This is the
   measurement instrument; without it the run is worthless.
6. **Nothing unsaved anywhere.** Arm B may make the keyboard unusable until replugged.
7. **LUKS is not at risk** and never is — the initramfs uses `hid-generic` and contains no
   `razerkbd`. A reboot always recovers.

### Assumption to verify in Phase 1, before any of this

`hidraw` feature reports must reach the device while `razerkbd` is the bound HID driver. `hidraw`
sits at the HID core level so this should hold, but it is an assumption, not a fact, until a Phase 1
read (firmware version / serial) returns a sane value with `razerkbd` loaded. **If it does not, stop
and re-plan** — do not work around it by unbinding anything.

---

## Method

For each arm, 20 iterations, 30 s apart:

1. Note `dmesg` timestamp and the device's current USB path and device number.
2. Send the report for that arm via our tool, with the explicit hardware opt-in flag.
3. Wait 30 s.
4. Record: any `usb <path>: USB disconnect`, the delay from write to disconnect, and whether the
   device re-enumerated on its own or needed a replug.

Run the arms in the order **A, C, D, B** — baseline first, the safe arms next, and the one that
deliberately breaks the keyboard last, so a mid-run abort costs the least.

### Recording

Per trial: arm, iteration, write timestamp, disconnect timestamp (or none), delta, old/new USB
device number, recovery (self / replug). Raw `dmesg` capture retained as the primary evidence.

---

## What counts as proof

- **Confirmed** if arm B produces ≥1 disconnect and arms A, C and D produce zero across 20 trials
  each. That is a reproducible, controlled A/B on a fault three upstream issues describe and none
  diagnose.
- **Not confirmed** if arm C also disconnects — then the transaction ID is not the mechanism and
  the report goes back to the drawing board. Say so; do not quietly drop the arm.
- **Competing hypothesis alive** if arm D disconnects — the poll-rate write is implicated instead
  of, or as well as, the mode write.

Correlation is not the deliverable here. The A/B *is* the mechanism, because the only variable
between arms B and C is one byte.

---

## After a confirmed result

1. Patch `openrazer` — route `razer_attr_write_device_mode()` and
   `razer_attr_read_device_mode()` through the existing per-PID switch rather than hardcoding
   `0xFF`. Mirror the shape `razermouse_driver.c:3864-3924` already uses. Note the clone is at
   `v3.12.1-41-g6820f9da` while the installed package is `3.12.4` — **fetch upstream first** or the
   patch lands on the wrong base.
2. Build with `make driver`, swap live with `rmmod razerkbd && insmod driver/razerkbd.ko`, re-run
   arms B and C against the patched module. The packaged DKMS module stays on disk, so a reboot
   always returns to known-good.
3. **Re-test the original symptom, not just the new code path** — full reboot, ly, login, daemon
   running normally, zero disconnects. This is precisely the step the 2026-08-03 fix skipped, and
   skipping it is how that fix escaped as broken.
4. Upstream PR referencing #2264, #2377, #2408 with the A/B data.
5. Update the Obsidian incident report (still `status: doing`, and its recorded remediation no
   longer exists on disk) and add a `Changes/arch-dan.md` entry for anything applied.
