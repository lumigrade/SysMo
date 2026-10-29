/* reporter.rs - sysmo-reporter: SysMo's Network tab reporter without a
 * window, for computers nobody logs in to, such as render or compute nodes.
 *
 * It runs the sensor engine, samples it once a second, and answers any
 * SysMo on the local network that shows the Network tab, exactly like the
 * app does. It needs no display and no GTK. tools/install-nodes.sh copies
 * it to nodes and has it started at boot.
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

// The engine and network code are the app's own files; parts only the app
// uses go unused here.
#![allow(dead_code)]

#[path = "config.rs"]
mod config;
#[path = "engine/mod.rs"]
mod engine;
#[path = "lan.rs"]
mod lan;

use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// At boot the reporter can start before the GPU driver is ready, and the
/// engine only looks for GPUs when it starts. While it reads fewer GPUs than
/// the computer has, the engine is restarted after each of these waits (in
/// seconds), then left alone.
const GPU_RECHECKS: [u64; 7] = [15, 30, 60, 120, 240, 480, 960];

fn main() -> ExitCode {
    // Stop cleanly on shutdown or Ctrl+C, so the engine process goes too.
    let stop = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        let _ = signal_hook::flag::register(signal, stop.clone());
    }

    let reporter = match lan::Reporter::start(lan::PORT) {
        Ok(reporter) => reporter,
        Err(e) => {
            eprintln!("sysmo-reporter: cannot use UDP port {}: {e}", lan::PORT);
            return ExitCode::FAILURE;
        }
    };

    let installed = engine::installed_gpus();
    let mut reading = 0;
    let mut rechecks = GPU_RECHECKS.iter().map(|secs| Duration::from_secs(*secs));
    let mut next_check = rechecks
        .next()
        .filter(|_| installed > 0)
        .map(|wait| Instant::now() + wait);

    let (sender, samples) = async_channel::unbounded();
    let mut engine = engine::Engine::start(sender.clone());
    while !stop.load(Ordering::Relaxed) {
        let started = Instant::now();
        engine.request_sample();
        // Wait for the reading in short steps, so a stop request is seen at
        // once even when the engine is slow to answer.
        while !stop.load(Ordering::Relaxed) && started.elapsed() < Duration::from_secs(1) {
            match samples.try_recv() {
                Ok(readings) => {
                    let load = engine::node_load(&readings);
                    reporter.update(load.cpu, load.gpu, load.gpus);
                    reading = engine::gpus_with_load(&readings);
                }
                Err(async_channel::TryRecvError::Empty) => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(async_channel::TryRecvError::Closed) => return ExitCode::FAILURE,
            }
        }

        if next_check.is_some_and(|at| Instant::now() >= at) {
            if reading >= installed {
                next_check = None;
            } else {
                eprintln!(
                    "sysmo-reporter: reading {reading} of {installed} GPUs, \
                     restarting the engine to look again"
                );
                // The node keeps reporting its last load meanwhile.
                drop(engine);
                engine = engine::Engine::start(sender.clone());
                reading = 0;
                next_check = rechecks.next().map(|wait| Instant::now() + wait);
            }
        }
    }

    // Stops the engine process.
    drop(engine);
    ExitCode::SUCCESS
}
