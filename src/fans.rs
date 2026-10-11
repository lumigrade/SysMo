/* fans.rs - motherboard fan headers with speed control, through the kernel's
 * hwmon interface (/sys/class/hwmon/hwmonN/pwmM). Used by the cooler panel
 * to run the CPU fan at a fixed speed. Writing needs the udev rule in
 * data/70-sysmo.rules.
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// pwmM_enable value for manual control (hwmon sysfs ABI).
const MANUAL: &str = "1";

#[derive(Clone)]
pub struct Header {
    /// Survives reboots, unlike hwmonN: "<chip>:<channel>", e.g. "nct6799:2".
    pub id: String,
    pub label: String,
    dir: PathBuf,
    channel: u32,
}

impl Header {
    fn file(&self, suffix: &str) -> PathBuf {
        self.dir.join(format!("pwm{}{suffix}", self.channel))
    }

    pub fn rpm(&self) -> Option<u32> {
        let path = self.dir.join(format!("fan{}_input", self.channel));
        fs::read_to_string(path).ok()?.trim().parse().ok()
    }
}

/// Every fan header whose speed can be set.
pub fn headers() -> Vec<Header> {
    headers_in(Path::new("/sys/class/hwmon"))
}

fn headers_in(root: &Path) -> Vec<Header> {
    let mut headers = Vec::new();
    let Ok(entries) = fs::read_dir(root) else {
        return headers;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        let Ok(chip) = fs::read_to_string(dir.join("name")) else {
            continue;
        };
        let chip = chip.trim();
        let Ok(files) = fs::read_dir(&dir) else {
            continue;
        };
        for file in files.flatten() {
            let name = file.file_name();
            let Some(channel) = name
                .to_str()
                .and_then(|name| name.strip_prefix("pwm"))
                .and_then(|n| n.parse::<u32>().ok())
            else {
                continue;
            };
            if dir.join(format!("pwm{channel}_enable")).exists() {
                headers.push(Header {
                    id: format!("{chip}:{channel}"),
                    label: format!("Fan {channel}"),
                    dir: dir.clone(),
                    channel,
                });
            }
        }
    }
    headers.sort_by(|a, b| a.id.cmp(&b.id));
    headers
}

/// Manual control of one header. Dropping it, or calling `release`, hands
/// the header back to the motherboard.
pub struct Control {
    header: Header,
    /// The header's pwmM_enable before SysMo took over.
    auto_mode: String,
    percent: Option<u8>,
}

impl Control {
    /// Take manual control. `saved_auto_mode` is the motherboard mode saved
    /// the last time, used if the header is already manual (SysMo stopped
    /// without handing it back).
    pub fn take(header: &Header, saved_auto_mode: Option<&str>) -> io::Result<Self> {
        let current = fs::read_to_string(header.file("_enable"))?
            .trim()
            .to_owned();
        let auto_mode = if current != MANUAL {
            current
        } else if let Some(saved) = saved_auto_mode.filter(|mode| *mode != MANUAL) {
            saved.to_owned()
        } else {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "the fan is already under manual control by another program",
            ));
        };
        fs::write(header.file("_enable"), MANUAL)?;
        Ok(Self {
            header: header.clone(),
            auto_mode,
            percent: None,
        })
    }

    pub fn header_id(&self) -> &str {
        &self.header.id
    }

    pub fn auto_mode(&self) -> &str {
        &self.auto_mode
    }

    pub fn set_percent(&mut self, percent: u8) -> io::Result<()> {
        if self.percent == Some(percent) {
            return Ok(());
        }
        let value = (percent.min(100) as u32 * 255 + 50) / 100;
        fs::write(self.header.file(""), value.to_string())?;
        self.percent = Some(percent);
        Ok(())
    }

    pub fn release(&mut self) -> io::Result<()> {
        fs::write(self.header.file("_enable"), &self.auto_mode)
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake hwmon chip with fan 2 under motherboard control (mode 5).
    fn fake_chip(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("sysmo-fans-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let chip = root.join("hwmon3");
        fs::create_dir_all(&chip).unwrap();
        fs::write(chip.join("name"), "nct6799\n").unwrap();
        fs::write(chip.join("pwm2"), "120\n").unwrap();
        fs::write(chip.join("pwm2_enable"), "5\n").unwrap();
        fs::write(chip.join("pwm2_mode"), "1\n").unwrap();
        fs::write(chip.join("fan2_input"), "1150\n").unwrap();
        root
    }

    fn read(root: &Path, file: &str) -> String {
        fs::read_to_string(root.join("hwmon3").join(file)).unwrap()
    }

    #[test]
    fn finds_controllable_headers() {
        let root = fake_chip("find");
        let headers = headers_in(&root);
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].id, "nct6799:2");
        assert_eq!(headers[0].rpm(), Some(1150));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn takes_and_hands_back_control() {
        let root = fake_chip("control");
        let header = &headers_in(&root)[0];
        {
            let mut control = Control::take(header, None).unwrap();
            assert_eq!(control.auto_mode(), "5");
            assert_eq!(read(&root, "pwm2_enable"), "1");
            control.set_percent(60).unwrap();
            assert_eq!(read(&root, "pwm2"), "153");
            control.set_percent(100).unwrap();
            assert_eq!(read(&root, "pwm2"), "255");
        }
        // Dropped: back to the motherboard's mode.
        assert_eq!(read(&root, "pwm2_enable"), "5");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn left_in_manual_uses_the_saved_mode() {
        let root = fake_chip("saved");
        let header = &headers_in(&root)[0];
        fs::write(root.join("hwmon3/pwm2_enable"), "1").unwrap();
        assert!(Control::take(header, None).is_err());
        let control = Control::take(header, Some("5")).unwrap();
        assert_eq!(control.auto_mode(), "5");
        drop(control);
        assert_eq!(read(&root, "pwm2_enable"), "5");
        fs::remove_dir_all(root).unwrap();
    }
}
