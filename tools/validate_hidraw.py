#!/usr/bin/env python3
"""
Validate the hidraw assumption for openrazer_rust, WITHOUT the Rust tool.

Rung 0 (zero device I/O): open the hidraw node and issue HIDIOCGRAWINFO, which reads
                          kernel-cached descriptor data. No packet reaches the device.
Rung 1 (benign device I/O): send the firmware-version and serial GET requests with the
                          CORRECT transaction id 0x1F and compare against the values
                          razerkbd already reports via sysfs.

Rung 1 sends the SAME commands openrazer-daemon issues routinely at every device init.
It does NOT send device_mode, and never uses transaction id 0xFF.

Ground truth for this machine (read from razerkbd sysfs 2026-09-04):
    firmware_version = v1.4
    device_serial    = TESTSERIAL00001

Usage:  sudo python3 validate_hidraw.py           # rung 0 only
        sudo python3 validate_hidraw.py --rung1   # rung 0 + rung 1
"""
import fcntl, struct, glob, os, sys, time

VID, PID = 0x1532, 0x028D
TRANSACTION_ID = 0x1F          # correct value for the BlackWidow V4 Pro
REPORT_LEN = 90
BUF_LEN = REPORT_LEN + 1       # hidraw prefixes the report number (0x00)
WAIT_S = 0.0006                # RAZER_BLACKWIDOW_CHROMA_WAIT_US = 600us

def _ioc(d, t, nr, size):
    v = (d << 30) | (size << 16) | (t << 8) | nr
    return v - (1 << 32) if v >= (1 << 31) else v

HIDIOCGRAWINFO  = _ioc(2, ord('H'), 0x03, 8)
HIDIOCSFEATURE  = _ioc(3, ord('H'), 0x06, BUF_LEN)
HIDIOCGFEATURE  = _ioc(3, ord('H'), 0x07, BUF_LEN)

def crc(report):
    """XOR bytes 2..87 inclusive — razercommon.c:111-124"""
    c = 0
    for i in range(2, 88):
        c ^= report[i]
    return c

def build(command_class, command_id, data_size, args=b''):
    """90-byte struct razer_report — razercommon.h:124-140"""
    r = bytearray(REPORT_LEN)
    r[0] = 0x00                      # status
    r[1] = TRANSACTION_ID            # transaction_id
    struct.pack_into('>H', r, 2, 0)  # remaining_packets, BIG-ENDIAN
    r[4] = 0x00                      # protocol_type
    r[5] = data_size
    r[6] = command_class
    r[7] = command_id
    r[8:8 + len(args)] = args        # arguments[80]
    r[88] = crc(r)
    r[89] = 0x00                     # reserved
    return bytes(r)

def find_node(iface_wanted):
    for node in sorted(glob.glob('/dev/hidraw*')):
        sysdev = '/sys/class/hidraw/%s/device' % os.path.basename(node)
        try:
            ue = open(os.path.join(sysdev, 'uevent')).read().upper()
        except Exception:
            continue
        if '%04X:%08X' % (VID, PID) not in ue:
            continue
        real = os.path.realpath(sysdev)
        usb_iface = real.split('/')[-2]          # e.g. 1-5.3:1.3
        try:
            n = int(open('/sys/bus/usb/devices/%s/bInterfaceNumber' % usb_iface).read().strip())
        except Exception:
            continue
        if n == iface_wanted:
            return node, usb_iface
    return None, None

def rung0(node):
    fd = os.open(node, os.O_RDWR)
    try:
        buf = bytearray(8)
        fcntl.ioctl(fd, HIDIOCGRAWINFO, buf, True)
        bus, vid, pid = struct.unpack('IhH', bytes(buf))
        print('  RUNG 0  ioctl OK   bus=0x%02x vid=%04x pid=%04x   (no packet sent to device)'
              % (bus, vid & 0xffff, pid))
        return True
    finally:
        os.close(fd)

def send_recv(node, command_class, command_id, data_size):
    fd = os.open(node, os.O_RDWR)
    try:
        out = bytearray(b'\x00' + build(command_class, command_id, data_size))
        fcntl.ioctl(fd, HIDIOCSFEATURE, out, True)
        time.sleep(max(WAIT_S, 0.01))
        inp = bytearray(BUF_LEN)
        fcntl.ioctl(fd, HIDIOCGFEATURE, inp, True)
        return bytes(inp[1:])
    finally:
        os.close(fd)

STATUS = {0x00: 'NEW', 0x01: 'BUSY', 0x02: 'SUCCESSFUL', 0x03: 'FAILURE',
          0x04: 'TIMEOUT', 0x05: 'NOT_SUPPORTED'}

def rung1(node):
    ok = True
    resp = send_recv(node, 0x00, 0x81, 0x02)          # firmware version
    st = resp[0]
    fw = 'v%d.%d' % (resp[8], resp[9])
    print('  RUNG 1  firmware      status=0x%02x %-12s -> %s' % (st, STATUS.get(st, '?'), fw))
    ok &= (st == 0x02)

    resp = send_recv(node, 0x00, 0x82, 0x16)          # serial
    st = resp[0]
    serial = resp[8:8 + 22].split(b'\x00')[0].decode('ascii', 'replace')
    print('  RUNG 1  serial        status=0x%02x %-12s -> %s' % (st, STATUS.get(st, '?'), serial))
    ok &= (st == 0x02)

    print()
    truth_fw = truth_sn = None
    import glob as g
    for d in g.glob('/sys/bus/hid/drivers/razerkbd/*028D*'):
        try:
            truth_fw = open(os.path.join(d, 'firmware_version')).read().strip()
            truth_sn = open(os.path.join(d, 'device_serial')).read().strip()
            break
        except Exception:
            continue
    print('  CROSS-CHECK vs razerkbd sysfs:')
    print('    firmware  ours=%-8s razerkbd=%-8s  %s' % (fw, truth_fw, 'MATCH' if fw == truth_fw else 'MISMATCH'))
    print('    serial    ours=%-18s razerkbd=%-18s  %s' % (serial, truth_sn, 'MATCH' if serial == truth_sn else 'MISMATCH'))
    return ok and fw == truth_fw and serial == truth_sn

if __name__ == '__main__':
    node, iface = find_node(3)
    if not node:
        print('FAIL: no hidraw node found for %04x:%04x interface 3' % (VID, PID)); sys.exit(1)
    print('BlackWidow V4 Pro -> %s (USB interface %s)\n' % (node, iface))
    try:
        rung0(node)
    except PermissionError:
        print('  PERMISSION DENIED — run with sudo'); sys.exit(1)
    if '--rung1' in sys.argv:
        print()
        good = rung1(node)
        print('\nVERDICT: %s' % ('hidraw transport PROVEN, encoding+CRC independently validated'
                                 if good else 'NOT proven — see above, do not proceed to Phase 3'))
    else:
        print('\n  (rung 1 not run — pass --rung1 to send the benign read commands)')
