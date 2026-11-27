//! One status line drawn from a gix progress tree while a transfer runs.

use std::collections::HashMap;
use std::io::Stderr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use gix::progress::tree::Root;
use jj_cli::ui::ProgressOutput;

const INITIAL_DELAY: Duration = Duration::from_millis(250);
const INTERVAL: Duration = Duration::from_millis(100);
/// Rates average over roughly this many intervals, so a stalled link shows up
/// within a few seconds without the number jumping on every redraw.
const RATE_SMOOTHING: f64 = 0.1;
/// A finished phase with no known total (the pack read) stops moving; drop it
/// once a later phase moves, but keep it while it is the only one.
const IDLE: Duration = Duration::from_secs(2);

/// Draws the tasks of a progress tree on one terminal line until dropped.
pub(crate) struct Display {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Display {
    /// Without a progress-capable terminal, nothing is drawn.
    pub(crate) fn start(output: Option<ProgressOutput<Stderr>>, root: Arc<Root>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let thread = output.map(|output| {
            let stop = stop.clone();
            std::thread::spawn(move || draw(output, &root, &stop))
        });
        Self { stop, thread }
    }
}

impl Drop for Display {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.join().ok();
        }
    }
}

struct Rate {
    step: usize,
    at: Instant,
    moved_at: Instant,
    per_second: Option<f64>,
}

fn draw(mut output: ProgressOutput<Stderr>, root: &Root, stop: &AtomicBool) {
    let started = Instant::now();
    let mut tasks = Vec::new();
    let mut rates = HashMap::new();
    let mut drawn = false;
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(INTERVAL);
        if started.elapsed() < INITIAL_DELAY {
            continue;
        }
        root.sorted_snapshot(&mut tasks);
        let now = Instant::now();
        let mut parts = Vec::new();
        for (key, task) in &tasks {
            let Some(value) = &task.progress else {
                continue;
            };
            let step = value.step.load(Ordering::Relaxed);
            if step == 0 || value.done_at == Some(step) {
                continue;
            }
            let rate = rates.entry(*key).or_insert(Rate {
                step,
                at: now,
                moved_at: now,
                per_second: None,
            });
            let elapsed = now.duration_since(rate.at).as_secs_f64();
            if elapsed > 0.0 {
                let sample = step.saturating_sub(rate.step) as f64 / elapsed;
                rate.per_second = Some(rate.per_second.map_or(sample, |previous| {
                    previous + RATE_SMOOTHING * (sample - previous)
                }));
                if step != rate.step {
                    rate.moved_at = now;
                }
                rate.step = step;
                rate.at = now;
            }
            let label = value.unit.as_ref().map_or_else(String::new, |unit| {
                let mut label = String::new();
                unit.as_display_value().display_unit(&mut label, step).ok();
                label
            });
            let mut part = format!("{} {}", task.name, amount(step, &label));
            if let Some(total) = value.done_at {
                part.push_str(&format!("/{}", amount(total, &label)));
            }
            if label != "B" && !label.is_empty() {
                part.push_str(&format!(" {label}"));
            }
            // Steps advance in jumps; a rate of them says nothing.
            if label != "steps"
                && let Some(per_second) = rate.per_second
            {
                part.push_str(&format!(" ({}/s)", amount(per_second as usize, &label)));
            }
            parts.push((now.duration_since(rate.moved_at) < IDLE, part));
        }
        if parts.iter().any(|(moving, _)| *moving) {
            parts.retain(|(moving, _)| *moving);
        }
        if parts.is_empty() {
            continue;
        }
        let mut line = parts
            .into_iter()
            .map(|(_, part)| part)
            .collect::<Vec<_>>()
            .join(", ");
        if let Some(width) = output.term_width() {
            // Leave the last column free so the terminal never wraps the line.
            if let Some((cut, _)) = line
                .char_indices()
                .nth(usize::from(width).saturating_sub(1))
            {
                line.truncate(cut);
            }
        }
        write!(output, "\r{line}\x1b[K").ok();
        output.flush().ok();
        drawn = true;
    }
    if drawn {
        write!(output, "\r\x1b[K").ok();
        output.flush().ok();
    }
}

/// Bytes in binary units, other counts with k/M suffixes; the caller adds
/// any other label once per task.
fn amount(value: usize, label: &str) -> String {
    let value = value as f64;
    if label == "B" {
        const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
        let exponent = ((value.max(1.0).log2() / 10.0) as usize).min(UNITS.len() - 1);
        let scaled = value / 1024f64.powi(exponent as i32);
        return if exponent == 0 {
            format!("{value} B")
        } else {
            format!("{scaled:.1} {}", UNITS[exponent])
        };
    }
    if value >= 1e6 {
        format!("{:.1}M", value / 1e6)
    } else if value >= 1e4 {
        format!("{:.0}k", value / 1e3)
    } else {
        format!("{value}")
    }
}
