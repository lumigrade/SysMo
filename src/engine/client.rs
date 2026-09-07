/* engine/client.rs
 *
 * IPC client for the Mission Center "magpie" engine. Trimmed down from
 * Mission Center's magpie_client (Copyright 2025 Romeo Calota, GPL-3.0-or-later)
 * to the handful of requests SysMo needs.
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use arrayvec::ArrayString;
use gtk::glib::{g_critical, g_debug, g_warning};

use magpie_types::cpu::{cpu_response, Cpu};
use magpie_types::disks::disks_response::DiskList;
use magpie_types::disks::{disks_response, Disk};
use magpie_types::gpus::gpus_response::GpuMap;
use magpie_types::gpus::{gpus_response, Gpu};
use magpie_types::ipc::{self, response};
use magpie_types::memory::memory_response::MemoryInfo;
use magpie_types::memory::{memory_request, memory_response, Memory, MemoryDevice};
use magpie_types::network::connections_response::ConnectionList;
use magpie_types::network::{connections_response, Connection};
use magpie_types::prost::Message;

use crate::config;

mod nng {
    pub use nng_c_sys::nng_errno_enum::*;
}

type ResponseBody = response::Body;
type CpuResponse = cpu_response::Response;
type DisksResponse = disks_response::Response;
type GpusResponse = gpus_response::Response;
type MemoryResponse = memory_response::Response;
type ConnectionsResponse = connections_response::Response;

/// Point SysMo at an already running engine instead of spawning one
/// (development aid, mirrors Mission Center's MC_DEBUG_MAGPIE_PROCESS_SOCK).
const ENV_EXISTING_SOCKET: &str = "SYSMO_ENGINE_SOCKET";

fn fatal(message: &str) -> ! {
    g_critical!("SysMo::Engine", "{}", message);
    eprintln!("sysmo: {message}");
    std::process::exit(1);
}

macro_rules! parse_response {
    ($response: ident, $body_kind: path, $response_kind_ok: path, $response_kind_err: path, $do: expr) => {{
        let expected_type = stringify!($response_kind_ok);
        match $response {
            Some($body_kind(response)) => match response.response {
                Some($response_kind_ok(arg)) => $do(arg),
                Some($response_kind_err(e)) => {
                    g_critical!(
                        "SysMo::Engine",
                        "Error while getting {}: {:?}",
                        expected_type,
                        e
                    );
                    Default::default()
                }
                _ => {
                    g_critical!(
                        "SysMo::Engine",
                        "Unexpected response: {:?}",
                        response.response
                    );
                    Default::default()
                }
            },
            _ => {
                g_critical!("SysMo::Engine", "Unexpected response: {:?}", $response);
                Default::default()
            }
        }
    }};
}

/// Sockets left behind by an instance that was killed rather than closed.
/// SysMo is single-instance, so any existing socket of ours is stale.
fn remove_stale_sockets() {
    let Ok(entries) = std::fs::read_dir("/tmp") else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("sysmo_") && name.ends_with(".ipc") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn random_string<const CAP: usize>() -> ArrayString<CAP> {
    let mut result = ArrayString::new();
    for _ in 0..CAP {
        if rand::random::<bool>() {
            result.push(rand::random_range(b'a'..=b'z') as char);
        } else {
            result.push(rand::random_range(b'0'..=b'9') as char);
        }
    }
    result
}

fn engine_command(socket_addr: &str) -> std::process::Command {
    let executable = config::engine_path();
    g_debug!(
        "SysMo::Engine",
        "Engine executable: {}",
        executable.display()
    );

    let mut command = std::process::Command::new(executable);
    command
        .env_remove("LD_PRELOAD")
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .arg("--addr")
        .arg(socket_addr);

    if std::env::var_os("MC_MAGPIE_HW_DB").is_none() {
        if let Some(hwdb) = config::hwdb_path() {
            command.env("MC_MAGPIE_HW_DB", hwdb);
        }
    }

    command
}

fn connect_socket(socket: &mut nng_c::Socket, socket_addr: &str) -> bool {
    let _ = socket.close();
    socket.id = 0;

    let new_socket = match nng_c::Socket::req0().map_err(|e| e.raw_code()) {
        Ok(s) => s,
        Err(error_code) => {
            let msg = match error_code {
                nng::NNG_ENOMEM => "Out of memory".to_string(),
                nng::NNG_ENOTSUP => "Protocol not supported".to_string(),
                _ => format!("Unknown error: {error_code}"),
            };
            g_critical!("SysMo::Engine", "Failed to open socket: {msg}");
            return false;
        }
    };

    *socket = new_socket;

    match socket
        .connect(nng_c::str::String::new(socket_addr.as_bytes()))
        .map_err(|e| e.raw_code())
    {
        Ok(_) => true,
        Err(error_code) => {
            let msg = match error_code {
                nng::NNG_EADDRINVAL => "An invalid url was specified".to_string(),
                nng::NNG_ECLOSED => "The socket is not open".to_string(),
                nng::NNG_ECONNREFUSED => "The remote peer refused the connection".to_string(),
                nng::NNG_ECONNRESET => "The remote peer reset the connection".to_string(),
                nng::NNG_EINVAL => {
                    "An invalid set of flags or an invalid url was specified".to_string()
                }
                nng::NNG_ENOMEM => "Insufficient memory is available".to_string(),
                nng::NNG_EPEERAUTH => "Authentication or authorization failure".to_string(),
                nng::NNG_EPROTO => "A protocol error occurred".to_string(),
                nng::NNG_EUNREACHABLE => "The remote address is not reachable".to_string(),
                _ => format!("Unknown error: {error_code}"),
            };
            g_critical!("SysMo::Engine", "Failed to dial socket: {msg}");
            false
        }
    }
}

fn make_request(
    request: ipc::Request,
    socket: &mut nng_c::Socket,
    socket_addr: &str,
) -> Option<ipc::Response> {
    fn try_reconnect(socket: &mut nng_c::Socket, socket_addr: &str) {
        socket.close();

        for i in 0..=5 {
            if !connect_socket(socket, socket_addr) {
                g_critical!(
                    "SysMo::Engine",
                    "Failed to reconnect to the engine. Retrying in 100ms (try {}/5)",
                    i + 1
                );
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            return;
        }

        fatal("Lost connection to the engine and failed to reconnect after 5 tries. Giving up.");
    }

    let mut req_buf = Vec::new();
    if let Err(e) = request.encode(&mut req_buf) {
        g_critical!("SysMo::Engine", "Failed to encode request: {}", e);
        return None;
    }

    if let Err(error_code) = socket
        .send(nng_c::socket::Buf::from(req_buf.as_slice()))
        .map_err(|e| e.raw_code())
    {
        match error_code {
            nng::NNG_ECLOSED => {
                g_critical!("SysMo::Engine", "Failed to send request: socket not open");
                try_reconnect(socket, socket_addr);
            }
            nng::NNG_ETIMEDOUT => {
                g_warning!("SysMo::Engine", "Failed to send request: timed out");
            }
            _ => {
                g_critical!(
                    "SysMo::Engine",
                    "Failed to send request: error code {error_code}"
                );
            }
        }
        return None;
    }

    let message = match socket.recv_msg().map_err(|e| e.raw_code()) {
        Ok(buffer) => buffer,
        Err(error_code) => {
            match error_code {
                nng::NNG_ECLOSED => {
                    g_critical!("SysMo::Engine", "Failed to read message: socket not open");
                    try_reconnect(socket, socket_addr);
                }
                nng::NNG_ETIMEDOUT => {
                    g_debug!("SysMo::Engine", "No message received, retrying later");
                    std::thread::sleep(Duration::from_millis(10));
                }
                _ => {
                    g_critical!(
                        "SysMo::Engine",
                        "Failed to read message: error code {error_code}"
                    );
                }
            }
            return None;
        }
    };

    if message.body().is_empty() {
        g_critical!("SysMo::Engine", "Failed to read response: empty message");
        return None;
    }

    match ipc::Response::decode(message.body()) {
        Ok(response) => Some(response),
        Err(e) => {
            g_critical!("SysMo::Engine", "Error while decoding response: {:?}", e);
            None
        }
    }
}

pub struct Client {
    socket: RefCell<nng_c::Socket>,
    socket_addr: Arc<str>,
    child_thread: RefCell<std::thread::JoinHandle<()>>,
    stop_requested: Arc<AtomicBool>,
}

impl Drop for Client {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Client {
    pub fn new() -> Self {
        let socket_addr: Arc<str> = match std::env::var(ENV_EXISTING_SOCKET) {
            Ok(mut existing) => {
                existing.push('\0');
                Arc::from(existing)
            }
            Err(_) => {
                remove_stale_sockets();
                Arc::from(format!("ipc:///tmp/sysmo_{}.ipc\0", random_string::<8>()))
            }
        };

        let socket = nng_c::Socket::req0().expect("Could not create initial socket");

        Self {
            socket: RefCell::new(socket),
            socket_addr,
            child_thread: RefCell::new(std::thread::spawn(|| {})),
            stop_requested: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn start(&self) {
        fn start_engine_process_thread(
            socket_addr: Arc<str>,
            stop_requested: Arc<AtomicBool>,
        ) -> std::thread::JoinHandle<()> {
            std::thread::spawn(move || {
                fn spawn_child(socket_addr: &str) -> std::process::Child {
                    match engine_command(socket_addr.trim_end_matches('\0')).spawn() {
                        Ok(child) => child,
                        Err(e) => fatal(&format!(
                            "Failed to spawn the engine process ({}): {}",
                            config::engine_path().display(),
                            e
                        )),
                    }
                }

                let mut child = spawn_child(&socket_addr);

                while !stop_requested.load(Ordering::Relaxed) {
                    match child.try_wait() {
                        Ok(Some(exit_status)) => {
                            let _ = std::fs::remove_file(&socket_addr[6..]);
                            if !stop_requested.load(Ordering::Relaxed) {
                                g_critical!(
                                    "SysMo::Engine",
                                    "Engine process exited unexpectedly: {}. Restarting...",
                                    exit_status
                                );
                                std::mem::swap(&mut child, &mut spawn_child(&socket_addr));
                            }
                        }
                        Ok(None) => {
                            std::thread::sleep(Duration::from_millis(100));
                            continue;
                        }
                        Err(e) => fatal(&format!("Failed to wait for the engine process: {}", e)),
                    }
                }

                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&socket_addr[6..].trim_end_matches('\0'));
            })
        }

        if std::env::var(ENV_EXISTING_SOCKET).is_err() {
            *self.child_thread.borrow_mut() =
                start_engine_process_thread(self.socket_addr.clone(), self.stop_requested.clone());
        }

        const START_WAIT_TIME_MS: u64 = 300;
        const RETRY_COUNT: i32 = 50;

        for _ in 0..RETRY_COUNT {
            std::thread::sleep(Duration::from_millis(START_WAIT_TIME_MS / 2));
            if connect_socket(&mut self.socket.borrow_mut(), &self.socket_addr) {
                return;
            }
            std::thread::sleep(Duration::from_millis(START_WAIT_TIME_MS / 2));
        }

        fatal("Failed to connect to the engine socket");
    }

    pub fn stop(&self) {
        self.stop_requested.store(true, Ordering::Relaxed);
        let child_thread = std::mem::replace(
            &mut *self.child_thread.borrow_mut(),
            std::thread::spawn(|| {}),
        );
        let _ = child_thread.join();
    }

    pub fn cpu(&self) -> Cpu {
        let mut socket = self.socket.borrow_mut();
        let response = make_request(ipc::req_get_cpu(), &mut socket, self.socket_addr.as_ref())
            .and_then(|response| response.body);

        parse_response!(
            response,
            ResponseBody::Cpu,
            CpuResponse::Cpu,
            CpuResponse::Error,
            |cpu: Cpu| cpu
        )
    }

    pub fn memory(&self) -> Memory {
        let mut socket = self.socket.borrow_mut();
        let response = make_request(
            ipc::req_get_memory(memory_request::Kind::Memory),
            &mut socket,
            self.socket_addr.as_ref(),
        )
        .and_then(|response| response.body);

        parse_response!(
            response,
            ResponseBody::Memory,
            MemoryResponse::MemoryInfo,
            MemoryResponse::Error,
            |memory: MemoryInfo| {
                let Some(memory_response::memory_info::Response::Memory(memory)) = memory.response
                else {
                    g_critical!("SysMo::Engine", "Unexpected response when getting memory");
                    return Default::default();
                };
                memory
            }
        )
    }

    pub fn memory_devices(&self) -> Vec<MemoryDevice> {
        let mut socket = self.socket.borrow_mut();
        let response = make_request(
            ipc::req_get_memory(memory_request::Kind::MemoryDevices),
            &mut socket,
            self.socket_addr.as_ref(),
        )
        .and_then(|response| response.body);

        parse_response!(
            response,
            ResponseBody::Memory,
            MemoryResponse::MemoryInfo,
            MemoryResponse::Error,
            |memory: MemoryInfo| {
                let Some(memory_response::memory_info::Response::MemoryDevices(mut devices)) =
                    memory.response
                else {
                    g_critical!(
                        "SysMo::Engine",
                        "Unexpected response when getting memory devices"
                    );
                    return Vec::new();
                };
                std::mem::take(&mut devices.devices)
            }
        )
    }

    pub fn disks(&self) -> Vec<Disk> {
        let mut socket = self.socket.borrow_mut();
        let response = make_request(ipc::req_get_disks(), &mut socket, self.socket_addr.as_ref())
            .and_then(|response| response.body);

        parse_response!(
            response,
            ResponseBody::Disks,
            DisksResponse::Disks,
            DisksResponse::Error,
            |mut disks: DiskList| { std::mem::take(&mut disks.disks) }
        )
    }

    pub fn connections(&self) -> HashMap<String, Connection> {
        let mut socket = self.socket.borrow_mut();
        let response = make_request(
            ipc::req_get_connections(),
            &mut socket,
            self.socket_addr.as_ref(),
        )
        .and_then(|response| response.body);

        parse_response!(
            response,
            ResponseBody::Connections,
            ConnectionsResponse::Connections,
            ConnectionsResponse::Error,
            |mut connections: ConnectionList| { std::mem::take(&mut connections.connections) }
        )
    }

    pub fn gpus(&self) -> HashMap<String, Gpu> {
        let mut socket = self.socket.borrow_mut();
        let response = make_request(ipc::req_get_gpus(), &mut socket, self.socket_addr.as_ref())
            .and_then(|response| response.body);

        parse_response!(
            response,
            ResponseBody::Gpus,
            GpusResponse::Gpus,
            GpusResponse::Error,
            |mut gpus: GpuMap| { std::mem::take(&mut gpus.gpus) }
        )
    }
}
