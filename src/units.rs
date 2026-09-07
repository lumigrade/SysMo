/* units.rs - value formatting that matches GNOME System Monitor. */

use gtk::glib;

/// Memory sizes: SI units ("6.7 GB"), like System Monitor's default.
pub fn bytes_si(bytes: u64) -> String {
    glib::format_size(bytes).to_string()
}

/// Transfer volumes and rates: IEC units ("2.4 KiB"), like System Monitor.
pub fn bytes_iec(bytes: u64) -> String {
    glib::format_size_full(bytes, glib::FormatSizeFlags::IEC_UNITS).to_string()
}

pub fn rate(bytes_per_second: u64) -> String {
    format!("{}/s", bytes_iec(bytes_per_second))
}

pub fn percent(fraction: f64) -> String {
    format!("{:.1}%", fraction * 100.0)
}

pub fn temperature(celsius: f32) -> String {
    format!("{:.0} °C", celsius)
}

fn plural(count: u32, singular: &str, plural: &str) -> String {
    if count == 1 {
        format!("{count} {singular}")
    } else {
        format!("{count} {plural}")
    }
}

/// Time-axis captions ("1 min", "50 secs"); a port of System Monitor's
/// `format_duration`. Zero seconds yields an empty caption.
pub fn duration(seconds: u32) -> String {
    let mut seconds = seconds;
    let mut minutes = seconds / 60;
    let mut hours = seconds / 3600;

    if hours != 0 {
        if minutes % 60 == 0 {
            minutes = 0;
        } else {
            minutes = ((seconds as f64 / 60.0).round() as u32) % 60;
            if minutes == 0 {
                hours += 1;
                seconds = hours * 3600;
            }
        }
    }

    let mut parts = Vec::with_capacity(3);
    if hours > 0 {
        parts.push(plural(hours, "hr", "hrs"));
    }
    if minutes > 0 {
        parts.push(plural(minutes, "min", "mins"));
    }
    if seconds % 60 > 0 {
        parts.push(plural(seconds % 60, "sec", "secs"));
    }
    parts.join(" ")
}
