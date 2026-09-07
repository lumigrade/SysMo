/* engine/mod.rs
 *
 * Runs the Mission Center engine (magpie) in a child process and samples it
 * from a background thread on request. Samples are delivered to the UI
 * through an async channel.
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

use std::collections::HashMap;
use std::sync::mpsc;
use std::thread::JoinHandle;

use gtk::glib::g_warning;

mod client;
use client::Client;

pub use magpie_types::cpu::Cpu;
pub use magpie_types::disks::Disk;
pub use magpie_types::gpus::Gpu;
pub use magpie_types::memory::{Memory, MemoryDevice};
pub use magpie_types::network::Connection;

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
