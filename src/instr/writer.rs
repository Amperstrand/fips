//! The capture writer: a dedicated, named OS thread with an explicit
//! lifecycle.
//!
//! There is no worker-thread lifecycle in this codebase to copy — the crypto
//! worker pools are never torn down and drop their join handles at spawn — so
//! this one is designed here.
//!
//! Two properties matter:
//!
//! - **It is an OS thread, not a spawned task.** The runtime is
//!   `current_thread`, so file I/O on a task would run on the rx loop's own
//!   thread and stall every `select!` arm, including the one being measured.
//! - **It waits on a channel with a timeout, not on a sleep.** `recv_timeout`
//!   returns immediately when the toggle sends stop, so `off` performs a final
//!   drain and joins promptly instead of parking the caller for up to a full
//!   flush interval.
//!
//! The thread is created lazily when a capture starts, so a node that never
//! arms one never has the thread.

use std::fs::File;
use std::io::Write;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{SystemTime, UNIX_EPOCH};

use super::capture::{self, BYTE_CAP, INTERVAL};
use super::recorder::{self, DOMAINS, GAUGES, STEPS};

/// Owner-side handle to the writer thread.
pub(crate) struct Handle {
    stop_tx: Sender<()>,
    join: JoinHandle<()>,
}

impl Handle {
    /// Wake the writer, let it drain once more, and join it.
    pub(crate) fn stop_and_join(self) {
        // A send error means the thread already exited (cap stop); joining is
        // still correct and returns at once.
        let _ = self.stop_tx.send(());
        let _ = self.join.join();
    }
}

/// Start the writer thread on an already-open sink.
pub(crate) fn spawn(file: File) -> std::io::Result<Handle> {
    let (stop_tx, stop_rx) = mpsc::channel();
    let join = thread::Builder::new()
        .name("fips-profile".to_string())
        .spawn(move || run(file, stop_rx))?;
    Ok(Handle { stop_tx, join })
}

fn run(mut file: File, stop_rx: Receiver<()>) {
    loop {
        match stop_rx.recv_timeout(INTERVAL) {
            // Stop requested, or the owner went away: final drain, then exit.
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                let _ = flush(&mut file);
                let _ = file.flush();
                return;
            }
            Err(RecvTimeoutError::Timeout) => {
                if flush(&mut file).is_err() {
                    // The sink is gone or full; stop the capture rather than
                    // spinning on a broken file for the rest of the run.
                    let _ = note(&mut file, "capture stopped: write error");
                    // The trailer above goes to the same failing file, so it is
                    // not a signal that survives. `stop` and a subsequent `on`
                    // both clear the state without surfacing it, so an operator
                    // would otherwise never learn the window was truncated.
                    tracing::warn!(
                        target: "fips::instr",
                        "profile capture stopped: write error on the sink"
                    );
                    capture::mark_stopped(capture::STOPPED_BY_ERROR);
                    return;
                }
                if capture::bytes_written() >= BYTE_CAP {
                    let _ = note(
                        &mut file,
                        &format!("capture stopped: byte cap {BYTE_CAP} reached"),
                    );
                    let _ = file.flush();
                    capture::mark_stopped(capture::STOPPED_BY_CAP);
                    return;
                }
            }
        }
    }
}

/// Append a `#`-prefixed trailer line.
fn note(file: &mut File, text: &str) -> std::io::Result<()> {
    let line = format!("# {text}\n");
    file.write_all(line.as_bytes())?;
    capture::add_bytes(line.len() as u64);
    Ok(())
}

/// Emit one interval: every step of every domain, then the gauges.
///
/// Every emitted step gets a row every interval, including zero-count rows, so
/// "this step did not run" is visible rather than absent. The two steps whose
/// call sites are conditionally compiled are excluded in builds that do not
/// have them, so no row is structurally zero forever.
fn flush(file: &mut File) -> std::io::Result<()> {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut out = String::with_capacity(4096);
    for domain in DOMAINS {
        for step in STEPS {
            if !step.emitted() {
                continue;
            }
            let (count, max_ns, total_ns) = recorder::take_step(domain, step);
            out.push_str(&format!(
                "{ts}\tstep\t{domain}\t{name}\t{count}\t{max}\t{total}\tus\n",
                domain = domain.name(),
                name = step.name(),
                max = max_ns / 1_000,
                total = total_ns / 1_000,
            ));
        }
    }
    // Gauges carry the tick domain today; the row kind and the unit column keep
    // them distinguishable from the duration rows above.
    for gauge in GAUGES {
        let (count, mut max, mut total) = recorder::take_gauge(gauge);
        if gauge.is_duration() {
            max /= 1_000;
            total /= 1_000;
        }
        out.push_str(&format!(
            "{ts}\tgauge\t{domain}\t{name}\t{count}\t{max}\t{total}\t{unit}\n",
            domain = recorder::Domain::Tick.name(),
            name = gauge.name(),
            unit = gauge.unit(),
        ));
    }

    file.write_all(out.as_bytes())?;
    capture::add_bytes(out.len() as u64);
    Ok(())
}
