use chrono::{Datelike, Local, Timelike};
use std::time::Duration;

pub fn fmt_hms_dur(d: Duration) -> String {
    let secs = d.as_secs();
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h:02}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

pub fn content_time_fmt(
    pos_samples: usize,
    total_samples: usize,
    sr: u32,
    ch: u16,
) -> (String, String) {
    if sr == 0 || ch == 0 {
        return ("00:00".into(), "00:00".into());
    }
    let pos = Duration::from_secs_f64(pos_samples as f64 / (sr as f64 * ch as f64));
    let tot = Duration::from_secs_f64(total_samples as f64 / (sr as f64 * ch as f64));
    (fmt_hms_dur(pos), fmt_hms_dur(tot))
}

pub fn now_stamp_ymdhms() -> String {
    let now = Local::now();
    format!(
        "{}{:02}{:02}_{:02}{:02}{:02}",
        now.year(),
        now.month(),
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    )
}
