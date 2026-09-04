# Emergency recovery

Driver work on a keyboard has an awkward property: the thing you break is the thing
you would use to fix it. This directory is the way back.

## The problem it solves

If a driver experiment leaves the keyboard unusable, you cannot type a `sudo` password,
so you cannot run the command that would repair it. A remote session can carry your
instruction but not your password.

So recovery is exposed as a systemd unit that a named user may start **without a password**,
authorised by polkit for exactly one user, one verb and one unit.

### The tradeoff you are accepting

This is a real privilege delegation, and it is worth naming rather than glossing:

- The unit runs **as root** — it has to, to `rmmod` and to write `/sys/bus/hid/drivers/*/bind`.
- The rule checks neither `subject.local` nor `subject.active`, deliberately: the entire point is
  that it works over SSH when there is no keyboard to log in at the console with.
- So **any** process running as that user can trigger it, not just an interactive session. A
  compromised browser tab or a malicious `npm postinstall` can start it with no prompt.

What bounds the damage is that the grant is to *one fixed unit* running *one fixed script*, not to
`systemctl` in general — so the worst an attacker gets is a keyboard driver rebind and a log line.
That is a good trade for a recovery path you can reach without a keyboard, but it is a trade. If
you would rather not make it, drop the polkit rule and use `sudo systemctl start
razer-recover.service`, accepting that you will need a working keyboard (or a second machine) to
type the password.

Keep `/usr/local/sbin/razer-recover` root-owned and mode 0755, as the install command below does:
the whole grant is only as narrow as that file is trustworthy.

## What it does

Best-effort and idempotent — safe to run when nothing is wrong:

1. Stops `openrazer-daemon` and `razerd` for every logged-in user, so nothing re-pokes the
   device mid-recovery.
2. Unloads `razerkbd` if present, with `rmmod` rather than `modprobe -r` — a
   `install <mod> /bin/true` line in `modprobe.d` makes `modprobe -r` silently succeed while
   doing nothing. **It does not reload it.** Recovery deliberately ends with the interfaces on
   `hid-generic` (step 3), because that is what makes the keyboard type; put `razerkbd` back
   with `sudo modprobe razerkbd`, or by rebooting, once you are done.
3. **Binds any orphaned Razer HID interface to `hid-generic`.** This is the important one. An
   interface bound to *no* driver is a dead keyboard, and it is precisely what happens when a
   module blacklist defeats OpenRazer's `razer_mount` udev helper part-way through: the helper
   unbinds from `hid-generic` first, then fails to bind to the blacklisted module.
4. Reports the final binding state and whether a keyboard event node exists.

## Install

```sh
sudo install -m 0755 razer-recover /usr/local/sbin/razer-recover
sudo install -m 0644 razer-recover.service /etc/systemd/system/
sudo systemctl daemon-reload

# edit the username in the rule first
sudo install -m 0644 49-razer-recover.rules /etc/polkit-1/rules.d/
```

## Use

```sh
systemctl start razer-recover.service        # no sudo, no password
journalctl -u razer-recover.service -n 40
```

## Test it before you need it

An untested recovery path is not a recovery path. Run it once while everything is healthy
and confirm it exits 0, reports every interface bound, and does not disturb a working
keyboard.

## What it cannot fix

- A device that needs a physical replug or power cycle. The script says so when it detects
  no keyboard event node.
- A kernel oops from a bad out-of-tree module. Reboot; the packaged DKMS module is still on
  disk and will load normally.
- Anything at all if the machine is not reachable. Have SSH working and **tested** first.
