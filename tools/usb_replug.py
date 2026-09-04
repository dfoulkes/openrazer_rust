#!/usr/bin/env python3
"""
Software replug for a Razer device, via its hub port's `disable` attribute.

WHAT IT IS FOR: on a BlackWidow V4 Pro the RGB goes dark at the point `razerkbd`
binds during boot and never comes back, however many times userspace repaints it.
A re-enumeration restores it reliably — five for five as of 2026-09-04. This does
that without reaching behind the desk.

It is a WORKAROUND, not a fix. The mechanism is not understood: paints issued
after the lights go out report success and change nothing, yet the identical call
works interactively. Tracked as an open issue.

WHAT DOES NOT WORK, so nobody wastes an evening rediscovering it:

  * USBDEVFS_RESET (the ioctl `usbreset` uses). It is a *port reset*: the device
    keeps its address, the kernel logs "reset full-speed USB device number N",
    and nothing changes. Tried 2026-09-04, no effect on the lights.
  * Repainting from userspace, however late or however often. Three paints at
    +12s, +18s and +25s after enumeration all reported success; the keyboard
    stayed dark.
  * Waiting longer before the first paint. Not a race.

A red herring worth recording: the keyboard enumerates at full speed (12 Mbps,
"not running at top speed") at boot and at high speed (480) after a physical
replug, which correlates perfectly with the lights. It is NOT causal — after a
software replug the lights come back while the link stays at 12 Mbps.

Writing 1 to <hub>:1.0/<hub>-portN/disable takes the port down as far as the hub
is concerned. Writing 0 brings it back and the device re-enumerates from scratch,
renegotiating speed — which is what a physical replug achieves.

THE PORT IS ALWAYS RE-ENABLED: on the happy path, on any exception, at
interpreter exit, and on SIGINT, SIGTERM or SIGHUP. Leaving a keyboard's port
disabled would be a genuinely nasty way to lose a machine.

SIGHUP is not paranoia. The whole point of this tool is doing a replug "without
reaching behind the desk" — i.e. over SSH — and a dropped SSH session delivers
exactly that signal. Handling only SIGINT would mean a flaky connection could
leave the port down until someone physically replugs it or reboots.
"""
import atexit, glob, os, signal, sys, time

VID, PID = '1532', '028d'


def find_device():
    for d in sorted(glob.glob('/sys/bus/usb/devices/*')):
        try:
            if open(os.path.join(d, 'idVendor')).read().strip() != VID:
                continue
            if open(os.path.join(d, 'idProduct')).read().strip() != PID:
                continue
            return (os.path.basename(d),
                    int(open(os.path.join(d, 'devnum')).read()),
                    open(os.path.join(d, 'speed')).read().strip())
        except (FileNotFoundError, ValueError):
            continue
    return None, None, None


def port_disable_path(devname):
    """Map e.g. '1-5.3' -> the hub port's disable attribute.

    A device named <parent>.<port> hangs off hub <parent>; a device named
    <bus>-<port> hangs off that bus's root hub.
    """
    if '.' in devname:
        parent, port = devname.rsplit('.', 1)
    else:
        bus, port = devname.split('-', 1)
        parent = f'{bus}-0'
    p = f'/sys/bus/usb/devices/{parent}:1.0/{parent}-port{port}/disable'
    return p if os.path.exists(p) else None


def main():
    name, devnum, speed = find_device()
    if name is None:
        print("  keyboard not found"); return 1
    print(f"  device   {name}  devnum={devnum}  speed={speed} Mbps")

    path = port_disable_path(name)
    if path is None:
        print(f"  no port disable attribute for {name} — cannot software-replug"); return 1
    print(f"  port     {path}")

    if speed == '480':
        print("  already at high speed — nothing to prove. Aborting.")
        return 0

    # Idempotent, because several paths can reach it: the `finally` below, the
    # atexit hook, and a signal handler that fires during either. Writing '0' to
    # an already-enabled port is harmless, but saying so twice is confusing.
    state = {'disabled': False}

    def reenable(*_):
        if not state['disabled']:
            return
        try:
            with open(path, 'w') as f:
                f.write('0')
            state['disabled'] = False
            print("  port re-enabled")
        except Exception as e:
            print(f"  !!! FAILED TO RE-ENABLE PORT: {e}")
            print(f"  !!! run: echo 0 | sudo tee {path}")
            print("  !!! or replug the keyboard by hand")

    def bail(signum, _frame):
        reenable()
        sys.exit(128 + signum)

    # Every way this process can be told to stop. SIGHUP is the one that matters
    # for the over-SSH use case; SIGTERM covers `kill` and systemd. Without
    # these, the default action terminates the process mid-sleep with the
    # keyboard's port still down.
    for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        signal.signal(sig, bail)
    # And a last-resort net for any exit path the above miss (including
    # SystemExit raised from inside a handler).
    atexit.register(reenable)

    try:
        print("  disabling port (keyboard will disappear) ...")
        with open(path, 'w') as f:
            # Set before the write, not after: once the fd is open, assume the
            # port may be down and always re-enable. (If the *open* failed we
            # never got here, so the flag stays False and reenable() correctly
            # stays quiet instead of printing a scary failure for a port that
            # was never disabled.)
            state['disabled'] = True
            f.write('1')
        time.sleep(3)
    finally:
        reenable()

    print("  waiting for re-enumeration ...")
    for i in range(25):
        time.sleep(1)
        n, newnum, newspeed = find_device()
        if n is not None and newnum != devnum:
            print(f"  back after {i+1}s: devnum {devnum} -> {newnum}, speed {newspeed} Mbps")
            if newspeed == '480':
                print("  *** HIGH SPEED NEGOTIATED — check the lights")
            else:
                print(f"  *** still {newspeed} Mbps — speed is not fixed by re-enumeration alone")
            return 0
    print("  did not come back within 25s — replug the keyboard by hand")
    return 1


if __name__ == '__main__':
    sys.exit(main())
