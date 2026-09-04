// SPDX-License-Identifier: GPL-2.0-or-later
//!
//! Prometheus exposition for `razerd`.
//!
//! # Design notes
//!
//! **No dependencies.** The text exposition format is a dozen lines of
//! `writeln!`, and a scrape endpoint is a `TcpListener` and a fixed response.
//! Pulling a metrics framework and an async runtime in for twenty series would
//! be more supply chain than signal.
//!
//! **Two classes of metric, gathered differently.**
//!
//! * *Host-observable* — device presence, USB device number, which kernel
//!   driver owns the HID interfaces. These come from `/sys` and cost the device
//!   nothing, so they are re-read on every scrape.
//! * *Device-interrogated* — firmware, device mode, poll rate, brightness.
//!   These are real HID transactions. They are polled on an interval and
//!   **cached**, because hammering a keyboard every 15 s to answer a scrape is
//!   rude and, on this hardware, not obviously safe.
//!
//! **Privacy.** The device serial is *not* exported by default. It is the one
//! genuinely identifying value a Razer device carries, and metrics have a habit
//! of ending up in screenshots and shared dashboards. `--expose-serial` opts in.
//!
//! # Golden signals
//!
//! Mapped deliberately, not retrofitted:
//!
//! | Signal | Metric |
//! |---|---|
//! | Latency | `razer_transaction_duration_seconds` |
//! | Traffic | `razer_transactions_total` |
//! | Errors | `razer_transaction_errors_total` |
//! | Saturation | `razer_transaction_attempts_total` vs `razer_transactions_total` |
//!
//! Saturation is the interesting one. A HID device has no queue depth to
//! measure, but the driver retries a rejected report up to five times. The
//! ratio of attempts to transactions is therefore a direct read on how hard the
//! device is having to be asked — which is exactly what degrades before a
//! firmware reset.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io::{BufRead, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use razer_hid::HardwareOptIn;

/// Largest request line (`GET /metrics HTTP/1.1`) we will buffer.
///
/// Without a bound, `read_line` on a socket that never sends a newline grows a
/// `String` until the process dies. 8 KiB is what nginx allows by default and is
/// three orders of magnitude more than any real scrape needs.
const MAX_REQUEST_LINE: u64 = 8 * 1024;

/// Largest single header line we will buffer, and how many we will read before
/// giving up. A Prometheus scrape sends about six.
const MAX_HEADER_LINE: u64 = 8 * 1024;
/// Maximum number of header lines read before the request is abandoned.
const MAX_HEADERS: usize = 64;

/// Read and write timeout for a scrape connection.
///
/// The accept loop is single-threaded, so a peer that connects and then says
/// nothing would otherwise park the only handler forever and starve every
/// subsequent scrape. This is the bound that makes serial handling safe.
const IO_TIMEOUT: Duration = Duration::from_secs(10);

/// Histogram buckets in seconds. A HID feature-report round trip is
/// sub-millisecond to a few milliseconds; the tail matters because the retry
/// path adds 10 ms per attempt.
const LATENCY_BUCKETS: &[f64] = &[
    0.0005, 0.001, 0.002, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0,
];

/// One device's identity, used as label values.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DeviceLabels {
    /// Model name from the device table, e.g. `Razer BlackWidow V4 Pro`.
    pub device: String,
    /// USB product id, rendered `0x028D`.
    pub pid: String,
}

impl DeviceLabels {
    fn render(&self) -> String {
        format!(
            "device=\"{}\",pid=\"{}\"",
            escape(&self.device),
            escape(&self.pid)
        )
    }
}

/// Escape a Prometheus label value.
///
/// The exposition format requires `\`, `"` and newline to be escaped. A bare
/// carriage return is not required to be, but these values come from the device
/// and from sysfs, so it gets the same treatment rather than landing raw in the
/// output.
fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

#[derive(Debug, Default, Clone)]
struct Histogram {
    counts: Vec<u64>,
    sum: f64,
    total: u64,
}

impl Histogram {
    fn new() -> Self {
        Self {
            counts: vec![0; LATENCY_BUCKETS.len()],
            sum: 0.0,
            total: 0,
        }
    }

    fn observe(&mut self, v: f64) {
        for (i, b) in LATENCY_BUCKETS.iter().enumerate() {
            if v <= *b {
                self.counts[i] += 1;
            }
        }
        self.sum += v;
        self.total += 1;
    }
}

/// Everything razerd knows about one device, refreshed by the poller.
#[derive(Debug, Default, Clone)]
pub struct DeviceState {
    pub present: bool,
    /// USB `devnum`. Increments on every re-enumeration, so
    /// `changes(razer_device_usb_devnum[1h])` counts disconnects.
    pub usb_devnum: Option<u64>,
    /// Which kernel HID driver owns the interfaces — `razerkbd` or `hid-generic`.
    pub kernel_driver: Option<String>,
    pub firmware: Option<String>,
    pub serial: Option<String>,
    pub device_mode: Option<u8>,
    pub poll_rate_hz: Option<u32>,
    pub brightness: Option<u8>,
    pub transaction_id: Option<u8>,
}

#[derive(Debug, Default)]
struct Inner {
    devices: BTreeMap<DeviceLabels, DeviceState>,
    transactions: BTreeMap<(DeviceLabels, String), u64>,
    attempts: BTreeMap<(DeviceLabels, String), u64>,
    errors: BTreeMap<(DeviceLabels, String), u64>,
    latency: BTreeMap<(DeviceLabels, String), Histogram>,
    poll_failures: u64,
    last_poll_duration: f64,
    polls: u64,
}

/// Thread-safe metric registry.
#[derive(Debug)]
pub struct Metrics {
    inner: Mutex<Inner>,
    expose_serial: bool,
    started: Instant,
}

impl Metrics {
    #[must_use]
    pub fn new(expose_serial: bool) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            expose_serial,
            started: Instant::now(),
        }
    }

    /// Record a completed transaction.
    ///
    /// `attempts` comes from `Session::last_attempts()`; `1` is a clean pass.
    pub fn record_transaction(
        &self,
        labels: &DeviceLabels,
        command: &str,
        attempts: u32,
        duration: Duration,
        error: Option<&str>,
    ) {
        let Ok(mut g) = self.inner.lock() else { return };
        let key = (labels.clone(), command.to_string());
        *g.transactions.entry(key.clone()).or_default() += 1;
        *g.attempts.entry(key.clone()).or_default() += u64::from(attempts);
        g.latency
            .entry(key)
            .or_insert_with(Histogram::new)
            .observe(duration.as_secs_f64());
        if let Some(reason) = error {
            *g.errors
                .entry((labels.clone(), reason.to_string()))
                .or_default() += 1;
        }
    }

    /// Replace a device's cached state after a poll.
    pub fn set_device_state(&self, labels: &DeviceLabels, state: DeviceState) {
        if let Ok(mut g) = self.inner.lock() {
            g.devices.insert(labels.clone(), state);
        }
    }

    pub fn record_poll(&self, duration: Duration, failed: bool) {
        if let Ok(mut g) = self.inner.lock() {
            g.polls += 1;
            g.last_poll_duration = duration.as_secs_f64();
            if failed {
                g.poll_failures += 1;
            }
        }
    }

    /// Render the exposition format.
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn render(&self) -> String {
        let mut o = String::with_capacity(4096);
        let Ok(g) = self.inner.lock() else {
            return String::from("# registry poisoned\n");
        };

        let _ = writeln!(o, "# HELP razer_build_info Build information.");
        let _ = writeln!(o, "# TYPE razer_build_info gauge");
        let _ = writeln!(
            o,
            "razer_build_info{{version=\"{}\"}} 1",
            escape(env!("CARGO_PKG_VERSION"))
        );

        let _ = writeln!(
            o,
            "# HELP razer_uptime_seconds Seconds since razerd started."
        );
        let _ = writeln!(o, "# TYPE razer_uptime_seconds gauge");
        let _ = writeln!(
            o,
            "razer_uptime_seconds {:.3}",
            self.started.elapsed().as_secs_f64()
        );

        // -- device identity and health ------------------------------------
        let _ = writeln!(
            o,
            "# HELP razer_device_info Device identity. Serial is only present when --expose-serial is set."
        );
        let _ = writeln!(o, "# TYPE razer_device_info gauge");
        for (l, s) in &g.devices {
            let mut extra = String::new();
            if let Some(f) = &s.firmware {
                let _ = write!(extra, ",firmware=\"{}\"", escape(f));
            }
            if let Some(t) = s.transaction_id {
                let _ = write!(extra, ",transaction_id=\"0x{t:02X}\"");
            }
            if self.expose_serial
                && let Some(sn) = &s.serial
            {
                let _ = write!(extra, ",serial=\"{}\"", escape(sn));
            }
            let _ = writeln!(o, "razer_device_info{{{}{}}} 1", l.render(), extra);
        }

        let _ = writeln!(
            o,
            "# HELP razer_device_present 1 if the device is currently enumerated."
        );
        let _ = writeln!(o, "# TYPE razer_device_present gauge");
        for (l, s) in &g.devices {
            let _ = writeln!(
                o,
                "razer_device_present{{{}}} {}",
                l.render(),
                u8::from(s.present)
            );
        }

        let _ = writeln!(
            o,
            "# HELP razer_device_usb_devnum USB device number. Increments on every re-enumeration, so changes() over a window counts USB disconnects."
        );
        let _ = writeln!(o, "# TYPE razer_device_usb_devnum gauge");
        for (l, s) in &g.devices {
            if let Some(n) = s.usb_devnum {
                let _ = writeln!(o, "razer_device_usb_devnum{{{}}} {n}", l.render());
            }
        }

        let _ = writeln!(
            o,
            "# HELP razer_device_kernel_driver Which in-kernel HID driver owns the interfaces."
        );
        let _ = writeln!(o, "# TYPE razer_device_kernel_driver gauge");
        for (l, s) in &g.devices {
            if let Some(d) = &s.kernel_driver {
                let _ = writeln!(
                    o,
                    "razer_device_kernel_driver{{{},driver=\"{}\"}} 1",
                    l.render(),
                    escape(d)
                );
            }
        }

        let _ = writeln!(
            o,
            "# HELP razer_device_mode Operating mode: 0 normal, 3 driver. Driver mode is the state in which the upstream razerkbd transaction-id defect resets this hardware."
        );
        let _ = writeln!(o, "# TYPE razer_device_mode gauge");
        for (l, s) in &g.devices {
            if let Some(m) = s.device_mode {
                let _ = writeln!(o, "razer_device_mode{{{}}} {m}", l.render());
            }
        }

        let _ = writeln!(o, "# HELP razer_device_poll_rate_hz Polling rate in hertz.");
        let _ = writeln!(o, "# TYPE razer_device_poll_rate_hz gauge");
        for (l, s) in &g.devices {
            if let Some(p) = s.poll_rate_hz {
                let _ = writeln!(o, "razer_device_poll_rate_hz{{{}}} {p}", l.render());
            }
        }

        let _ = writeln!(
            o,
            "# HELP razer_device_brightness_ratio Backlight brightness, 0-1."
        );
        let _ = writeln!(o, "# TYPE razer_device_brightness_ratio gauge");
        for (l, s) in &g.devices {
            if let Some(b) = s.brightness {
                let _ = writeln!(
                    o,
                    "razer_device_brightness_ratio{{{}}} {:.4}",
                    l.render(),
                    f64::from(b) / 255.0
                );
            }
        }

        // -- golden signals -------------------------------------------------
        let _ = writeln!(
            o,
            "# HELP razer_transactions_total TRAFFIC: completed HID transactions."
        );
        let _ = writeln!(o, "# TYPE razer_transactions_total counter");
        for ((l, cmd), v) in &g.transactions {
            let _ = writeln!(
                o,
                "razer_transactions_total{{{},command=\"{}\"}} {v}",
                l.render(),
                escape(cmd)
            );
        }

        let _ = writeln!(
            o,
            "# HELP razer_transaction_attempts_total SATURATION: attempts consumed, including retries. attempts/transactions > 1 means the device is rejecting well-formed reports."
        );
        let _ = writeln!(o, "# TYPE razer_transaction_attempts_total counter");
        for ((l, cmd), v) in &g.attempts {
            let _ = writeln!(
                o,
                "razer_transaction_attempts_total{{{},command=\"{}\"}} {v}",
                l.render(),
                escape(cmd)
            );
        }

        let _ = writeln!(
            o,
            "# HELP razer_transaction_errors_total ERRORS: failed transactions by reason."
        );
        let _ = writeln!(o, "# TYPE razer_transaction_errors_total counter");
        for ((l, reason), v) in &g.errors {
            let _ = writeln!(
                o,
                "razer_transaction_errors_total{{{},reason=\"{}\"}} {v}",
                l.render(),
                escape(reason)
            );
        }

        let _ = writeln!(
            o,
            "# HELP razer_transaction_duration_seconds LATENCY: HID feature-report round trip."
        );
        let _ = writeln!(o, "# TYPE razer_transaction_duration_seconds histogram");
        for ((l, cmd), h) in &g.latency {
            let base = format!("{},command=\"{}\"", l.render(), escape(cmd));
            for (i, b) in LATENCY_BUCKETS.iter().enumerate() {
                let _ = writeln!(
                    o,
                    "razer_transaction_duration_seconds_bucket{{{base},le=\"{b}\"}} {}",
                    h.counts[i]
                );
            }
            let _ = writeln!(
                o,
                "razer_transaction_duration_seconds_bucket{{{base},le=\"+Inf\"}} {}",
                h.total
            );
            let _ = writeln!(
                o,
                "razer_transaction_duration_seconds_sum{{{base}}} {:.6}",
                h.sum
            );
            let _ = writeln!(
                o,
                "razer_transaction_duration_seconds_count{{{base}}} {}",
                h.total
            );
        }

        // -- exporter self-observability ------------------------------------
        let _ = writeln!(o, "# HELP razer_polls_total Device poll cycles run.");
        let _ = writeln!(o, "# TYPE razer_polls_total counter");
        let _ = writeln!(o, "razer_polls_total {}", g.polls);
        let _ = writeln!(
            o,
            "# HELP razer_poll_failures_total Poll cycles that failed for at least one device."
        );
        let _ = writeln!(o, "# TYPE razer_poll_failures_total counter");
        let _ = writeln!(o, "razer_poll_failures_total {}", g.poll_failures);
        let _ = writeln!(
            o,
            "# HELP razer_poll_duration_seconds Duration of the most recent poll cycle."
        );
        let _ = writeln!(o, "# TYPE razer_poll_duration_seconds gauge");
        let _ = writeln!(o, "razer_poll_duration_seconds {:.6}", g.last_poll_duration);

        o
    }
}

/// Read `/sys` for the host-observable facts about a device: its USB device
/// number and which kernel driver owns its HID interfaces. Costs the device
/// nothing — no packet is sent.
///
/// Takes a [`HardwareOptIn`] even though it opens no device node. The
/// workspace's contract is that *every* path which reads `/sys` is gated, so
/// that one grep for the token's constructor answers "did this run go anywhere
/// near the hardware?". A `/sys` reader in `razerd` that skipped the gate would
/// make that answer wrong, which is worse than no gate at all.
#[must_use]
pub fn read_sysfs_state(
    hidraw_name: &str,
    pid: u16,
    _opt_in: &HardwareOptIn,
) -> (Option<u64>, Option<String>) {
    let devnum = usb_parent_dir(hidraw_name)
        .and_then(|d| fs::read_to_string(d.join("devnum")).ok())
        .and_then(|s| s.trim().parse::<u64>().ok());

    // Which driver owns the HID interfaces for this PID. All five normally
    // agree; if they ever disagree, report the first, which is enough to make
    // the split visible in a graph.
    let mut driver = None;
    if let Ok(entries) = fs::read_dir("/sys/bus/hid/devices") {
        let needle = format!(":{pid:04X}.");
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_uppercase();
            if !name.contains(&needle) {
                continue;
            }
            if let Ok(p) = fs::read_link(e.path().join("driver")) {
                driver = p.file_name().map(|n| n.to_string_lossy().into_owned());
                break;
            }
        }
    }
    (devnum, driver)
}

/// Walk up from a hidraw class entry to the USB device directory — the first
/// ancestor that has a `devnum`.
///
/// Private, and reachable only through [`read_sysfs_state`], which holds the
/// `HardwareOptIn` gate for both.
fn usb_parent_dir(hidraw_name: &str) -> Option<PathBuf> {
    let start = fs::canonicalize(
        Path::new("/sys/class/hidraw")
            .join(hidraw_name)
            .join("device"),
    )
    .ok()?;
    let mut cur: &Path = &start;
    loop {
        if cur.join("devnum").exists() {
            return Some(cur.to_path_buf());
        }
        cur = cur.parent()?;
        if cur.as_os_str().len() <= "/sys".len() {
            return None;
        }
    }
}

/// The default listen address.
///
/// Loopback, deliberately. The exposition carries device identity — and, with
/// `--expose-serial`, the one genuinely identifying value the hardware holds —
/// over an endpoint with no authentication of any kind. Binding it to the
/// network is a decision someone should have to make on purpose.
pub const DEFAULT_ADDR: &str = "127.0.0.1:9782";

/// Serve `/metrics` on `addr` until the process exits.
///
/// Single-threaded and deliberately so: a scrape is a handful of microseconds
/// of string formatting, Prometheus scrapes serially, and a thread pool would
/// be more machinery than the job needs. Serial handling is only safe because
/// every connection carries [`IO_TIMEOUT`] and bounded reads; without those, one
/// idle peer starves every subsequent scrape.
///
/// Anything other than a loopback address is logged loudly at startup, because
/// this endpoint has no authentication. See [`DEFAULT_ADDR`].
///
/// # Errors
///
/// Propagates the bind error if `addr` cannot be listened on.
pub fn serve(addr: &str, metrics: &Metrics) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr)?;
    let local = listener.local_addr()?;
    if !local.ip().is_loopback() {
        eprintln!(
            "razerd: WARNING: metrics listening on {local}, which is not loopback.\n\
             razerd:          This endpoint is unauthenticated and exposes device identity\n\
             razerd:          (and the serial, with --expose-serial) to anyone who can reach it."
        );
    }
    eprintln!("razerd: metrics on http://{local}/metrics");
    for stream in listener.incoming() {
        match stream {
            Ok(s) => handle(s, metrics),
            Err(e) => eprintln!("razerd: accept failed: {e}"),
        }
    }
    Ok(())
}

fn handle(mut stream: TcpStream, metrics: &Metrics) {
    // Before anything is read: a peer that connects and says nothing must not be
    // able to hold the only handler thread. Both directions, because a peer that
    // stops reading mid-response would otherwise block the write too.
    if stream.set_read_timeout(Some(IO_TIMEOUT)).is_err()
        || stream.set_write_timeout(Some(IO_TIMEOUT)).is_err()
    {
        return;
    }

    let mut path = String::new();
    {
        let mut reader = BufReader::new(match stream.try_clone() {
            Ok(s) => s,
            Err(_) => return,
        });
        // `take` is the bound: read_line on an unbounded reader grows the String
        // for as long as the peer sends bytes without a newline.
        let mut line = String::new();
        if (&mut reader)
            .take(MAX_REQUEST_LINE)
            .read_line(&mut line)
            .is_err()
        {
            return;
        }
        if line.len() as u64 >= MAX_REQUEST_LINE && !line.ends_with('\n') {
            // Oversized request line; answering it would mean guessing at a
            // truncated method and target.
            return;
        }
        if let Some(p) = line.split_whitespace().nth(1) {
            path = p.to_string();
        }
        // Drain the rest of the request headers, bounded in both count and size.
        for _ in 0..MAX_HEADERS {
            let mut h = String::new();
            match (&mut reader).take(MAX_HEADER_LINE).read_line(&mut h) {
                Ok(0) => break,
                Ok(_) if h.trim().is_empty() => break,
                Ok(_) => {}
                Err(_) => return,
            }
        }
    }

    let (status, ctype, body) = match path.as_str() {
        "/metrics" => (
            "200 OK",
            "text/plain; version=0.0.4; charset=utf-8",
            metrics.render(),
        ),
        "/" => (
            "200 OK",
            "text/html; charset=utf-8",
            String::from(
                "<html><body><h1>razerd</h1><a href=\"/metrics\">metrics</a></body></html>\n",
            ),
        ),
        _ => (
            "404 Not Found",
            "text/plain; charset=utf-8",
            String::from("not found\n"),
        ),
    };

    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels() -> DeviceLabels {
        DeviceLabels {
            device: "Razer BlackWidow V4 Pro".into(),
            pid: "0x028D".into(),
        }
    }

    #[test]
    fn serial_is_absent_unless_opted_in() {
        let m = Metrics::new(false);
        m.set_device_state(
            &labels(),
            DeviceState {
                present: true,
                serial: Some("IO2433F81500082".into()),
                ..DeviceState::default()
            },
        );
        let out = m.render();
        assert!(
            !out.contains("IO2433F81500082"),
            "serial must not leak without --expose-serial"
        );
        assert!(!out.contains("serial="));
    }

    #[test]
    fn serial_is_present_when_opted_in() {
        let m = Metrics::new(true);
        m.set_device_state(
            &labels(),
            DeviceState {
                present: true,
                serial: Some("TESTSERIAL00001".into()),
                ..DeviceState::default()
            },
        );
        assert!(m.render().contains("serial=\"TESTSERIAL00001\""));
    }

    #[test]
    fn attempts_exceed_transactions_when_retrying() {
        let m = Metrics::new(false);
        m.record_transaction(
            &labels(),
            "firmware_version",
            3,
            Duration::from_millis(5),
            None,
        );
        let out = m.render();
        assert!(out.contains("razer_transactions_total{device=\"Razer BlackWidow V4 Pro\",pid=\"0x028D\",command=\"firmware_version\"} 1"));
        assert!(out.contains("razer_transaction_attempts_total{device=\"Razer BlackWidow V4 Pro\",pid=\"0x028D\",command=\"firmware_version\"} 3"));
    }

    #[test]
    fn errors_are_counted_by_reason() {
        let m = Metrics::new(false);
        m.record_transaction(
            &labels(),
            "serial",
            5,
            Duration::from_millis(50),
            Some("retries_exhausted"),
        );
        assert!(m.render().contains("reason=\"retries_exhausted\"} 1"));
    }

    #[test]
    fn device_mode_and_devnum_render() {
        let m = Metrics::new(false);
        m.set_device_state(
            &labels(),
            DeviceState {
                present: true,
                usb_devnum: Some(11),
                kernel_driver: Some("razerkbd".into()),
                device_mode: Some(0x03),
                ..DeviceState::default()
            },
        );
        let out = m.render();
        assert!(out.contains(
            "razer_device_usb_devnum{device=\"Razer BlackWidow V4 Pro\",pid=\"0x028D\"} 11"
        ));
        assert!(out.contains("driver=\"razerkbd\"} 1"));
        assert!(
            out.contains("razer_device_mode{device=\"Razer BlackWidow V4 Pro\",pid=\"0x028D\"} 3")
        );
    }

    #[test]
    fn label_values_are_escaped() {
        let m = Metrics::new(true);
        m.set_device_state(
            &DeviceLabels {
                device: "Weird \"Device\"".into(),
                pid: "0x0000".into(),
            },
            DeviceState {
                present: true,
                ..DeviceState::default()
            },
        );
        assert!(m.render().contains("device=\"Weird \\\"Device\\\"\""));
    }

    #[test]
    fn carriage_returns_are_escaped_too() {
        // kernel_driver and firmware are device- and sysfs-sourced, so they get
        // the same treatment as anything else that lands in a label value.
        let m = Metrics::new(false);
        m.set_device_state(
            &labels(),
            DeviceState {
                present: true,
                kernel_driver: Some("razerkbd\r\nnot-a-new-series".into()),
                ..DeviceState::default()
            },
        );
        let out = m.render();
        assert!(out.contains(r"razerkbd\r\nnot-a-new-series"));
        assert!(
            !out.contains('\r'),
            "a raw carriage return reached the exposition output"
        );
    }

    /// Drive one request through `handle` on a real socket and return whatever
    /// the server said, or `None` if it hung up without answering.
    fn scrape(request: &[u8]) -> Option<String> {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let req = request.to_vec();
        let client = std::thread::spawn(move || {
            let mut c = TcpStream::connect(addr).expect("connect");
            c.set_read_timeout(Some(Duration::from_secs(10))).ok();
            c.write_all(&req).ok();
            let mut out = String::new();
            c.read_to_string(&mut out).ok();
            out
        });
        let (s, _) = listener.accept().expect("accept");
        handle(s, &Metrics::new(false));
        let out = client.join().expect("client thread");
        if out.is_empty() { None } else { Some(out) }
    }

    #[test]
    fn a_well_formed_scrape_is_answered() {
        let out = scrape(b"GET /metrics HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .expect("a normal scrape must be answered");
        assert!(out.starts_with("HTTP/1.1 200 OK"));
        assert!(out.contains("razer_build_info"));
    }

    #[test]
    fn a_request_line_with_no_newline_is_bounded_not_buffered_forever() {
        // The unbounded version of this grew a String for as long as the peer
        // kept sending. `take(MAX_REQUEST_LINE)` is what stops it, so the read
        // must terminate and the request must be refused rather than guessed at.
        let flood = vec![b'A'; (MAX_REQUEST_LINE as usize) * 2];
        assert!(
            scrape(&flood).is_none(),
            "an oversized request line must be refused, not answered"
        );
    }

    #[test]
    fn the_header_loop_is_bounded() {
        // Ten times MAX_HEADERS, never terminated by a blank line. The loop must
        // stop counting and answer rather than reading until the peer relents.
        let mut req = Vec::from(&b"GET /metrics HTTP/1.1\r\n"[..]);
        for i in 0..(MAX_HEADERS * 10) {
            req.extend_from_slice(format!("X-Pad-{i}: x\r\n").as_bytes());
        }
        let out = scrape(&req).expect("a bounded header run must still be answered");
        assert!(out.starts_with("HTTP/1.1 200 OK"));
    }

    #[test]
    fn the_default_address_is_loopback() {
        let addr: std::net::SocketAddr = DEFAULT_ADDR.parse().expect("DEFAULT_ADDR parses");
        assert!(
            addr.ip().is_loopback(),
            "an unauthenticated endpoint that can expose the device serial must \
             not default to a routable address"
        );
    }

    #[test]
    fn histogram_buckets_are_cumulative() {
        let m = Metrics::new(false);
        for _ in 0..3 {
            m.record_transaction(&labels(), "x", 1, Duration::from_millis(1), None);
        }
        let out = m.render();
        // 1ms observation falls in the 0.001 bucket and every larger one.
        assert!(out.contains("le=\"0.001\"} 3"));
        assert!(out.contains("le=\"0.01\"} 3"));
        assert!(out.contains("le=\"+Inf\"} 3"));
        assert!(out.contains("le=\"0.0005\"} 0"));
    }
}
