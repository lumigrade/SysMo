/* cooler.rs - shows CPU readings on the display of a DeepCool digital
 * cooler (AK700 DIGITAL and the other coolers that share its protocol).
 *
 * The packet layout is the "LQ Series / ASSASSIN IV / AK G2 Series / AK700"
 * mapping table of deepcool-digital-linux by Nortank12 (GPL-3.0); see
 * CREDITS.md. Writing to /dev/hidrawN needs the udev rule in
 * data/70-sysmo.rules.
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{mpsc, Arc};

const DEEPCOOL_VENDOR_ID: u32 = 0x3633;

/// Coolers that take this packet, by USB product ID.
const SUPPORTED: [(u32, &str); 7] = [
    (13, "LQ240 / LQ360"),
    (15, "ASSASSIN IV VC VISION"),
    (31, "ASSASSIN IV VC VISION"),
    (41, "AK620 G2 DIGITAL"),
    (42, "AK700 DIGITAL"),
    (43, "AK400 G2 DIGITAL"),
    (44, "AK500 G2 DIGITAL"),
];

pub struct Device {
    pub name: &'static str,
    node: PathBuf,
}

/// The first supported cooler that is plugged in. Reading sysfs needs no
/// permission, so this also finds a cooler SysMo is not yet allowed to use.
pub fn find() -> Option<Device> {
    let mut entries: Vec<_> = fs::read_dir("/sys/class/hidraw")
        .ok()?
        .flatten()
        .map(|entry| entry.file_name())
        .collect();
    entries.sort();

    for node in entries {
        let uevent = PathBuf::from("/sys/class/hidraw")
            .join(&node)
            .join("device/uevent");
        let Ok(uevent) = fs::read_to_string(uevent) else {
            continue;
        };
        // HID_ID=0003:00003633:0000002A (bus, vendor, product)
        let Some(id) = uevent.lines().find_map(|line| line.strip_prefix("HID_ID=")) else {
            continue;
        };
        let mut parts = id.split(':').skip(1);
        let (Some(vendor), Some(product)) = (parts.next(), parts.next()) else {
            continue;
        };
        if u32::from_str_radix(vendor, 16) != Ok(DEEPCOOL_VENDOR_ID) {
            continue;
        }
        let Ok(product) = u32::from_str_radix(product, 16) else {
            continue;
        };
        if let Some((_, name)) = SUPPORTED.iter().find(|(pid, _)| *pid == product) {
            return Some(Device {
                name,
                node: PathBuf::from("/dev").join(node),
            });
        }
    }
    None
}

/// One frame for the display.
pub struct Reading {
    pub power_watts: u16,
    /// Already in the unit chosen by `fahrenheit`.
    pub temperature: f32,
    pub fahrenheit: bool,
    pub usage_percent: u8,
    pub frequency_mhz: u16,
}

fn packet(reading: &Reading) -> [u8; 64] {
    let mut data = [0u8; 64];
    // Report ID, then a fixed header.
    data[..7].copy_from_slice(&[16, 104, 1, 8, 12, 1, 2]);
    data[7..9].copy_from_slice(&reading.power_watts.to_be_bytes());
    data[9] = reading.fahrenheit as u8;
    data[10..14].copy_from_slice(&reading.temperature.to_be_bytes());
    data[14] = reading.usage_percent.min(100);
    data[15..17].copy_from_slice(&reading.frequency_mhz.to_be_bytes());
    let checksum: u32 = data[1..=16].iter().map(|&byte| byte as u32).sum();
    data[17] = (checksum % 256) as u8;
    data[18] = 22; // termination byte
    data
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    /// Nothing sent yet.
    Idle,
    NotFound,
    /// The cooler is there but /dev/hidrawN is not writable: the udev rule
    /// is missing.
    NoPermission,
    Connected,
}

impl Link {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::NotFound,
            2 => Self::NoPermission,
            3 => Self::Connected,
            _ => Self::Idle,
        }
    }
}

/// Sends frames from a background thread, so a stalled USB write never
/// holds up the window.
pub struct Display {
    sender: mpsc::SyncSender<[u8; 64]>,
    link: Arc<AtomicU8>,
}

impl Display {
    pub fn start() -> Self {
        let (sender, receiver) = mpsc::sync_channel::<[u8; 64]>(1);
        let link = Arc::new(AtomicU8::new(Link::Idle as u8));

        std::thread::Builder::new()
            .name("sysmo-cooler".into())
            .spawn({
                let link = link.clone();
                move || {
                    let set = |state: Link| link.store(state as u8, Ordering::Relaxed);
                    let mut file: Option<File> = None;
                    while let Ok(frame) = receiver.recv() {
                        if file.is_none() {
                            let Some(device) = find() else {
                                set(Link::NotFound);
                                continue;
                            };
                            match OpenOptions::new().write(true).open(&device.node) {
                                Ok(opened) => file = Some(opened),
                                Err(e) if e.kind() == ErrorKind::PermissionDenied => {
                                    set(Link::NoPermission);
                                    continue;
                                }
                                Err(_) => {
                                    set(Link::NotFound);
                                    continue;
                                }
                            }
                        }
                        if let Some(device) = file.as_mut() {
                            if device.write_all(&frame).is_ok() {
                                set(Link::Connected);
                            } else {
                                // Unplugged or reset: look for it again next time.
                                file = None;
                                set(Link::NotFound);
                            }
                        }
                    }
                }
            })
            .expect("failed to spawn the cooler thread");

        Self { sender, link }
    }

    /// Queue a frame; dropped if the previous one is still being written.
    pub fn send(&self, reading: &Reading) {
        let _ = self.sender.try_send(packet(reading));
    }

    pub fn link(&self) -> Link {
        Link::from_u8(self.link.load(Ordering::Relaxed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_matches_the_mapping_table() {
        let data = packet(&Reading {
            power_watts: 65,
            temperature: 54.0,
            fahrenheit: false,
            usage_percent: 37,
            frequency_mhz: 4800,
        });
        assert_eq!(&data[..7], &[16, 104, 1, 8, 12, 1, 2]);
        assert_eq!(&data[7..9], &[0, 65]);
        assert_eq!(data[9], 0);
        assert_eq!(&data[10..14], &54.0f32.to_be_bytes());
        assert_eq!(data[14], 37);
        assert_eq!(&data[15..17], &[0x12, 0xc0]);
        let sum: u32 = data[1..=16].iter().map(|&b| b as u32).sum();
        assert_eq!(data[17] as u32, sum % 256);
        assert_eq!(data[18], 22);
        assert!(data[19..].iter().all(|&b| b == 0));
    }
}
