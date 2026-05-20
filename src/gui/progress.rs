//! Rolling-window transfer-rate + ETA estimator with a small egui widget.
//!
//! Adapted from the `rusty-backup` pattern (`src/gui/progress.rs` there).
//! Pair with an `Arc<Mutex<HashProgress>>` written from a worker thread.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Rolling-window sampler. Caller `record`s the current byte counter each
/// frame; the tracker exposes a smoothed rate and ETA.
#[derive(Default)]
pub struct RateTracker {
    samples: VecDeque<(Instant, u64)>,
    last_stage: String,
}

impl RateTracker {
    pub fn record(&mut self, current_bytes: u64, stage: &str) {
        const WINDOW: Duration = Duration::from_secs(10);
        if stage != self.last_stage {
            self.last_stage = stage.to_string();
            self.samples.clear();
        }
        if let Some(&(_, last_bytes)) = self.samples.back() {
            if current_bytes < last_bytes {
                self.samples.clear();
            }
        }
        let now = Instant::now();
        self.samples.push_back((now, current_bytes));
        while let Some(&(t, _)) = self.samples.front() {
            if now.duration_since(t) > WINDOW {
                self.samples.pop_front();
            } else {
                break;
            }
        }
    }

    pub fn rate_bytes_per_sec(&self) -> Option<f64> {
        if self.samples.len() < 2 {
            return None;
        }
        let (t_first, b_first) = *self.samples.front()?;
        let (t_last, b_last) = *self.samples.back()?;
        let dt = t_last.duration_since(t_first).as_secs_f64();
        if dt < 0.25 {
            return None;
        }
        let db = b_last.saturating_sub(b_first) as f64;
        if db == 0.0 {
            return Some(0.0);
        }
        Some(db / dt)
    }

    pub fn eta_secs(&self, current_bytes: u64, total_bytes: u64) -> Option<u64> {
        let rate = self.rate_bytes_per_sec()?;
        if rate <= 0.0 || total_bytes == 0 {
            return None;
        }
        let remaining = total_bytes.saturating_sub(current_bytes) as f64;
        Some((remaining / rate) as u64)
    }

    pub fn suffix(&self, current_bytes: u64, total_bytes: u64) -> String {
        match (
            self.rate_bytes_per_sec(),
            self.eta_secs(current_bytes, total_bytes),
        ) {
            (Some(r), Some(eta)) if r > 0.0 => {
                format!(" — {}/s, ETA {}", format_rate(r), format_eta(eta))
            }
            (Some(r), _) if r > 0.0 => format!(" — {}/s", format_rate(r)),
            _ => String::new(),
        }
    }

    pub fn reset(&mut self) {
        self.samples.clear();
        self.last_stage.clear();
    }
}

pub fn format_size(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let b = bytes as f64;
    if b >= GIB {
        format!("{:.2} GiB", b / GIB)
    } else if b >= MIB {
        format!("{:.1} MiB", b / MIB)
    } else if b >= KIB {
        format!("{:.0} KiB", b / KIB)
    } else {
        format!("{b:.0} B")
    }
}

fn format_rate(bytes_per_sec: f64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    if bytes_per_sec >= GIB {
        format!("{:.2} GiB", bytes_per_sec / GIB)
    } else if bytes_per_sec >= MIB {
        format!("{:.1} MiB", bytes_per_sec / MIB)
    } else if bytes_per_sec >= KIB {
        format!("{:.0} KiB", bytes_per_sec / KIB)
    } else {
        format!("{bytes_per_sec:.0} B")
    }
}

fn format_eta(total_secs: u64) -> String {
    let h = total_secs / 3600;
    let m = (total_secs % 3600) / 60;
    let s = total_secs % 60;
    if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}
