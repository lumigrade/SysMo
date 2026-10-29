/* engine/mod.rs
 *
 * Runs the Mission Center engine (magpie) in a child process and samples it
 * from a background thread on request. Samples are delivered to the UI
 * through an async channel.
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc;
use std::thread::JoinHandle;

use glib::g_warning;

mod client;
use client::Client;

pub use magpie_types::cpu::Cpu;
pub use magpie_types::disks::Disk;
pub use magpie_types::gpus::Gpu;
pub use magpie_types::memory::{Memory, MemoryDevice};
pub use magpie_types::network::Connection;

/// Integrated Intel graphics; hidden whenever a discrete GPU exists.
pub fn is_integrated(gpu: &Gpu) -> bool {
    gpu.vendor_id == 0x8086
}

/// NVIDIA and AMD graphics cards in this computer, from the PCI device
/// list, which needs no driver. The engine finds GPUs through their drivers,
/// and only when it starts. (Used by sysmo-reporter.)
#[allow(dead_code)]
pub fn installed_gpus() -> usize {
    count_gpus(Path::new("/sys/bus/pci/devices"))
}

fn count_gpus(pci_devices: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(pci_devices) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| {
            let read =
                |name: &str| std::fs::read_to_string(entry.path().join(name)).unwrap_or_default();
            // Class 0x03xxxx: a display controller (VGA, 3D or other).
            read("class").trim().starts_with("0x03")
                && matches!(read("vendor").trim(), "0x10de" | "0x1002")
        })
        .count()
}

/// GPUs whose load the engine reads, integrated Intel graphics left out.
/// (Used by sysmo-reporter.)
#[allow(dead_code)]
pub fn gpus_with_load(readings: &Readings) -> usize {
    readings
        .gpus
        .values()
        .filter(|gpu| !is_integrated(gpu) && gpu.utilization_percent.is_some())
        .count()
}

/// The GPUs SysMo shows, in a stable order.
pub fn displayed_gpus(gpus: &HashMap<String, Gpu>) -> Vec<&Gpu> {
    let has_discrete = gpus.values().any(|gpu| !is_integrated(gpu));
    let mut ordered: Vec<&Gpu> = gpus
        .values()
        .filter(|gpu| !(has_discrete && is_integrated(gpu)))
        .collect();
    ordered.sort_by_key(|gpu| gpu.id.clone());
    ordered
}

/// What the Network tab shows for a computer.
pub struct NodeLoad {
    pub cpu: f32,
    /// Average over the shown GPUs that report a load.
    pub gpu: Option<f32>,
    pub gpus: u32,
}

pub fn node_load(readings: &Readings) -> NodeLoad {
    let gpus = displayed_gpus(&readings.gpus);
    let loads: Vec<f32> = gpus
        .iter()
        .filter_map(|gpu| gpu.utilization_percent)
        .collect();
    NodeLoad {
        cpu: readings.cpu.total_usage_percent,
        gpu: (!loads.is_empty()).then(|| loads.iter().sum::<f32>() / loads.len() as f32),
        gpus: gpus.len() as u32,
    }
}

/// One snapshot of everything SysMo displays.
pub struct Readings {
    pub cpu: Cpu,
    pub memory: Memory,
    /// Installed memory modules; static, so only the first sample carries them.
    pub memory_devices: Option<Vec<MemoryDevice>>,
    pub disks: Vec<Disk>,
    pub connections: HashMap<String, Connection>,
    pub gpus: HashMap<String, Gpu>,
}

enum Command {
    Sample,
}

pub struct Engine {
    commands: mpsc::SyncSender<Command>,
    thread: Option<JoinHandle<()>>,
}

impl Engine {
    /// Spawn the engine process and the sampling thread. Every `Readings`
    /// produced in response to `request_sample` is sent on `output`.
    pub fn start(output: async_channel::Sender<Readings>) -> Self {
        // Capacity 1 so a slow engine never builds up a backlog of requests.
        let (commands, receiver) = mpsc::sync_channel::<Command>(1);

        let thread = std::thread::Builder::new()
            .name("sysmo-engine".into())
            .spawn(move || {
                let client = Client::new();
                client.start();

                // Prime the engine's rate counters; the very first request has
                // no baseline to compute speeds against.
                let _ = Self::gather(&client);

                let mut static_info_sent = false;
                while let Ok(Command::Sample) = receiver.recv() {
                    let mut readings = Self::gather(&client);
                    if !static_info_sent {
                        readings.memory_devices = Some(client.memory_devices());
                        static_info_sent = true;
                    }
                    if output.send_blocking(readings).is_err() {
                        break;
                    }
                }

                client.stop();
            })
            .expect("failed to spawn the engine thread");

        Self {
            commands,
            thread: Some(thread),
        }
    }

    pub fn request_sample(&self) {
        if let Err(mpsc::TrySendError::Disconnected(_)) = self.commands.try_send(Command::Sample) {
            g_warning!("SysMo::Engine", "Sampling thread is gone");
        }
    }

    fn gather(client: &Client) -> Readings {
        Readings {
            cpu: client.cpu(),
            memory: client.memory(),
            memory_devices: None,
            disks: client.disks(),
            connections: client.connections(),
            gpus: client.gpus(),
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        // Closing the command channel makes the thread's recv() fail, which
        // stops the engine process and ends the thread.
        let (dummy, _) = mpsc::sync_channel::<Command>(1);
        drop(std::mem::replace(&mut self.commands, dummy));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu(id: &str, vendor_id: u32, load: Option<f32>) -> Gpu {
        Gpu {
            id: id.into(),
            vendor_id,
            utilization_percent: load,
            ..Default::default()
        }
    }

    fn readings(cpu: f32, gpus: Vec<Gpu>) -> Readings {
        Readings {
            cpu: Cpu {
                total_usage_percent: cpu,
                ..Default::default()
            },
            memory: Memory::default(),
            memory_devices: None,
            disks: Vec::new(),
            connections: HashMap::new(),
            gpus: gpus.into_iter().map(|g| (g.id.clone(), g)).collect(),
        }
    }

    #[test]
    fn node_load_averages_the_shown_gpus() {
        let load = node_load(&readings(
            12.0,
            vec![
                gpu("a", 0x10de, Some(90.0)),
                gpu("b", 0x10de, Some(30.0)),
                // Integrated Intel graphics are left out next to a discrete GPU.
                gpu("i", 0x8086, Some(0.0)),
            ],
        ));
        assert_eq!(load.cpu, 12.0);
        assert_eq!(load.gpu, Some(60.0));
        assert_eq!(load.gpus, 2);
    }

    #[test]
    fn counts_nvidia_and_amd_graphics_cards() {
        let root = std::env::temp_dir().join(format!("sysmo-pci-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (address, class, vendor) in [
            ("0000:01:00.0", "0x030000", "0x10de"), // NVIDIA VGA
            ("0000:01:00.1", "0x040300", "0x10de"), // its HDMI audio
            ("0000:02:00.0", "0x030200", "0x10de"), // NVIDIA 3D controller
            ("0000:03:00.0", "0x038000", "0x1002"), // AMD display controller
            ("0000:00:02.0", "0x030000", "0x8086"), // Intel integrated
            ("0000:04:00.0", "0x020000", "0x10ec"), // network card
        ] {
            let device = root.join(address);
            std::fs::create_dir_all(&device).unwrap();
            std::fs::write(device.join("class"), format!("{class}\n")).unwrap();
            std::fs::write(device.join("vendor"), format!("{vendor}\n")).unwrap();
        }
        assert_eq!(count_gpus(&root), 3);
        assert_eq!(count_gpus(&root.join("missing")), 0);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn counts_gpus_with_a_load() {
        let r = readings(
            0.0,
            vec![
                gpu("a", 0x10de, Some(90.0)),
                gpu("b", 0x10de, None),
                gpu("i", 0x8086, Some(5.0)),
            ],
        );
        assert_eq!(gpus_with_load(&r), 1);
    }

    #[test]
    fn node_load_without_gpu_readings() {
        let none = node_load(&readings(5.0, Vec::new()));
        assert_eq!((none.gpu, none.gpus), (None, 0));
        // An Intel GPU on its own counts.
        let intel = node_load(&readings(5.0, vec![gpu("i", 0x8086, Some(40.0))]));
        assert_eq!((intel.gpu, intel.gpus), (Some(40.0), 1));
    }
}
