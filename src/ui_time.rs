pub fn format_clock(content_secs: u64, total_secs: u64) -> String {
    let (fmt_c, fmt_t) = if total_secs >= 3600 {
        (fmt_hms(content_secs), fmt_hms(total_secs))
    } else {
        (fmt_ms(content_secs), fmt_ms(total_secs))
    };
    format!("{} / {}", fmt_c, fmt_t)
}

fn fmt_ms(secs: u64) -> String {
    let m = secs / 60;
    let s = secs % 60;
    format!("{:02}:{:02}", m, s)
}

fn fmt_hms(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{:02}:{:02}:{:02}", h, m, s)
}
