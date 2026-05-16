//! Structured JSON-line BLE event logging for pcap/log timestamp correlation.
//!
//! When the `FIPS_BLE_EVENT_LOG` environment variable is set to a file path,
//! BLE lifecycle and measurement events are appended as one JSON object per
//! line (JSONL). Each entry carries an ISO 8601 UTC timestamp with microsecond
//! precision for correlation with btsnoop pcap (epoch 2000-01-01) and keylog
//! timestamps.
//!
//! When the env var is not set, all calls are zero-cost (no allocation, no I/O).

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::{Mutex, OnceLock};

// ---------------------------------------------------------------------------
// Singleton state
// ---------------------------------------------------------------------------

/// Sentinel: env var not set.
const DISABLED: &str = "";

static EVENT_LOG_PATH: OnceLock<&'static str> = OnceLock::new();

/// Returns the configured file path, or `DISABLED` ("") when not enabled.
fn event_log_path() -> &'static str {
    *EVENT_LOG_PATH.get_or_init(|| {
        // Leak the String to obtain a `'static` reference.
        // This is called at most once and the path must live for the process
        // lifetime anyway.
        std::env::var("FIPS_BLE_EVENT_LOG")
            .ok()
            .map(|s| Box::leak(s.into_boxed_str()) as &str)
            .unwrap_or(DISABLED)
    })
}

/// Open the event log file in append mode. Returns `None` on error.
fn open_event_log(path: &str) -> Option<std::fs::File> {
    OpenOptions::new().create(true).append(true).open(path).ok()
}

/// Global file handle behind a mutex. `None` means logging is disabled.
static EVENT_FILE: OnceLock<Mutex<Option<std::fs::File>>> = OnceLock::new();

/// Lazily initialise and return a reference to the global file mutex.
fn global_file() -> &'static Mutex<Option<std::fs::File>> {
    EVENT_FILE.get_or_init(|| {
        let path = event_log_path();
        if path.is_empty() {
            Mutex::new(None)
        } else {
            Mutex::new(open_event_log(path))
        }
    })
}

/// Returns `true` when event logging is enabled (env var was set and file was
/// opened successfully). Cheap enough to call on every event site.
pub fn is_enabled() -> bool {
    let path = event_log_path();
    if path.is_empty() {
        return false;
    }
    // If the global file hasn't been initialised yet, trigger init.
    let guard = global_file().lock().unwrap_or_else(|e| e.into_inner());
    guard.is_some()
}

// ---------------------------------------------------------------------------
// Timestamp formatting
// ---------------------------------------------------------------------------

/// Format the current wall-clock time as ISO 8601 UTC with microsecond
/// precision, e.g. `"2026-05-10T14:23:45.123456Z"`.
fn iso8601_now() -> String {
    let dur = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or(std::time::Duration::ZERO);

    let secs = dur.as_secs();
    let micros = dur.subsec_micros();

    // Gregorian calendar math (UTC). Days from 1970-01-01.
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hour = time_of_day / 3600;
    let minute = (time_of_day % 3600) / 60;
    let second = time_of_day % 60;

    // Convert days-since-epoch to year-month-day.
    let (year, month, day) = days_to_ymd(days as i64);

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:06}Z",
        year, month, day, hour, minute, second, micros
    )
}

/// Convert days since 1970-01-01 to (year, month, day).
fn days_to_ymd(mut days: i64) -> (i64, u8, u8) {
    // Shift to a 400-year cycle starting March 1, year 0 (calendar hack).
    days += 719468; // days from 0000-03-01 to 1970-01-01
    let era = if days >= 0 {
        days / 146097
    } else {
        (days - 146096) / 146097
    };
    let mut doe = days - era * 146097; // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // year of era [0, 399]
    let mut y = yoe + era * 400;
    doe -= yoe * 365 + yoe / 4 - yoe / 100;
    let mp = (doe * 10 + 5) / 306; // month [0-based from March]
    let d = doe - (mp * 306 + 5) / 10;
    y += (mp + 2) / 12;
    let m = ((mp + 2) % 12) as u8 + 1; // 1-based
    (y, m, (d + 1) as u8)
}

// ---------------------------------------------------------------------------
// JSON escaping (minimal, sufficient for our field values)
// ---------------------------------------------------------------------------

/// Escape a string for inclusion in a JSON value. Handles backslash, double
/// quote, and common control characters.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Append a structured event to the JSONL log file.
///
/// `fields` is a flat slice of `(key, value)` pairs. Values are emitted as
/// JSON strings. Numeric fields should be formatted by the caller.
///
/// Uses `try_lock()` to avoid blocking hot paths. If the mutex is contested,
/// the event is silently dropped.
pub fn log(event: &str, peer: &str, fields: &[(&str, &str)]) {
    let file_mutex = global_file();
    let mut guard = match file_mutex.try_lock() {
        Ok(g) => g,
        Err(_) => return, // contested — drop event
    };
    let Some(file) = guard.as_mut() else {
        return;
    };

    let ts = iso8601_now();

    // Build JSON line manually (avoids serde dependency overhead for this).
    let mut line = String::with_capacity(256);
    line.push_str("{\"ts\":\"");
    line.push_str(&ts);
    line.push_str("\",\"event\":\"");
    line.push_str(&json_escape(event));
    line.push_str("\",\"peer\":\"");
    line.push_str(&json_escape(peer));
    line.push_str("\"");
    for (k, v) in fields {
        line.push_str(",\"");
        line.push_str(&json_escape(k));
        line.push_str("\":\"");
        line.push_str(&json_escape(v));
        line.push_str("\"");
    }
    line.push_str("}\n");

    let _ = file.write_all(line.as_bytes());
    // Flush each event so timestamps are timely for correlation.
    let _ = file.flush();
}
