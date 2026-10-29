/* lan.rs - SysMo nodes on the local network.
 *
 * Every SysMo runs a reporter. A SysMo showing the Network tab runs a watcher
 * that broadcasts one "who's there?" a minute; each reporter answers and then
 * sends its CPU and GPU load to that watcher, one small datagram a second,
 * for as long as the watcher keeps asking (a lease). Nothing scans the
 * subnet, and nothing is sent while nobody watches.
 *
 * Messages are single UDP datagrams of UTF-8 text:
 *   SYSMO/1 DISCOVER lease=<seconds>                      watcher -> broadcast
 *   SYSMO/1 BYE                                           watcher -> reporter
 *   SYSMO/1 STATUS id=<hex> cpu=<%> gpu=<%|-> gpus=<n> name=<host>
 *                                                         reporter -> watcher
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const PORT: u16 = 47917;
/// How long a reporter keeps sending after a DISCOVER: a bit more than the
/// watcher's 60 s rounds, so one lost broadcast does not interrupt it.
pub const LEASE: Duration = Duration::from_secs(75);
/// A node is idle when both its CPU and GPU stay under this...
pub const IDLE_BELOW_PERCENT: f32 = 10.0;
/// ...on average over this long.
pub const IDLE_WINDOW: Duration = Duration::from_secs(30);

const MAGIC: &str = "SYSMO/1";
const MAX_LEASE: Duration = Duration::from_secs(300);
const MAX_WATCHERS: usize = 16;
const MAX_DATAGRAM: usize = 512;
const POLL: Duration = Duration::from_millis(200);

#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    /// Stable per machine, so a restarted SysMo keeps its row.
    pub id: String,
    pub name: String,
    pub cpu: f32,
    /// Average over the GPUs; `None` without a GPU reading.
    pub gpu: Option<f32>,
    pub gpus: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    Discover { lease: Duration },
    Bye,
    Status(Status),
}

impl Message {
    pub fn encode(&self) -> String {
        match self {
            Self::Discover { lease } => format!("{MAGIC} DISCOVER lease={}", lease.as_secs()),
            Self::Bye => format!("{MAGIC} BYE"),
            Self::Status(status) => {
                let gpu = status
                    .gpu
                    .map(|gpu| format!("{gpu:.1}"))
                    .unwrap_or_else(|| "-".to_owned());
                let name: String = status.name.chars().take(64).collect();
                format!(
                    "{MAGIC} STATUS id={} cpu={:.1} gpu={gpu} gpus={} name={name}",
                    status.id, status.cpu, status.gpus
                )
            }
        }
    }

    pub fn decode(text: &str) -> Option<Self> {
        let rest = text.strip_prefix(MAGIC)?.strip_prefix(' ')?;
        let (kind, fields) = rest.split_once(' ').unwrap_or((rest, ""));
        match kind {
            "DISCOVER" => {
                let lease = field(fields, "lease")?.parse().ok()?;
                Some(Self::Discover {
                    lease: Duration::from_secs(lease),
                })
            }
            "BYE" => Some(Self::Bye),
            "STATUS" => {
                // `name` is last and may hold anything but a newline.
                let (fields, name) = fields.split_once(" name=")?;
                let percent = |key| -> Option<f32> {
                    let value: f32 = field(fields, key)?.parse().ok()?;
                    value.is_finite().then_some(value.clamp(0.0, 100.0))
                };
                let id = field(fields, "id")?;
                if id.is_empty() || name.contains('\n') {
                    return None;
                }
                Some(Self::Status(Status {
                    id: id.to_owned(),
                    name: name.to_owned(),
                    cpu: percent("cpu")?,
                    gpu: percent("gpu"),
                    gpus: field(fields, "gpus")?.parse().ok()?,
                }))
            }
            _ => None,
        }
    }
}

fn field<'a>(fields: &'a str, key: &str) -> Option<&'a str> {
    fields.split(' ').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then_some(v)
    })
}

/// Only machines on the local network may ask for updates: private
/// addresses, and any address on a network this computer is directly on.
fn is_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || fs::read_to_string("/proc/net/route").is_ok_and(|table| on_link(&table, ip))
        }
        IpAddr::V6(_) => false,
    }
}

/// Whether `ip` is on a directly attached network, from the kernel's
/// /proc/net/route table (routes without a gateway; the default route is
/// left out).
fn on_link(table: &str, ip: Ipv4Addr) -> bool {
    let address = |hex: &str| {
        u32::from_str_radix(hex, 16)
            .ok()
            .map(|n| Ipv4Addr::from(n.to_ne_bytes()))
    };
    table.lines().skip(1).any(|line| {
        let columns: Vec<&str> = line.split_whitespace().collect();
        let (Some(destination), Some(gateway), Some(mask)) = (
            columns.get(1).and_then(|hex| address(hex)),
            columns.get(2).and_then(|hex| address(hex)),
            columns.get(7).and_then(|hex| address(hex)),
        ) else {
            return false;
        };
        let mask = u32::from(mask);
        gateway.is_unspecified()
            && mask != 0
            && u32::from(ip) & mask == u32::from(destination) & mask
    })
}

fn read_trimmed(path: &str) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

pub fn host_name() -> String {
    read_trimmed("/proc/sys/kernel/hostname").unwrap_or_else(|| "unknown".to_owned())
}

/// A hash of the machine id: stable across restarts, and the machine id
/// itself (meant to stay private) never leaves the computer.
pub fn node_id() -> String {
    let seed = read_trimmed("/etc/machine-id")
        .or_else(|| read_trimmed("/var/lib/dbus/machine-id"))
        .unwrap_or_else(host_name);
    // FNV-1a, 64 bit.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in "sysmo-node:".bytes().chain(seed.bytes()) {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[derive(Clone, Copy)]
struct Load {
    cpu: f32,
    gpu: Option<f32>,
    gpus: u32,
}

/// Answers watchers and sends them this computer's load once a second.
pub struct Reporter {
    #[cfg(test)]
    port: u16,
    load: Arc<Mutex<Option<Load>>>,
    stop: Arc<AtomicBool>,
}

impl Reporter {
    pub fn start(port: u16) -> io::Result<Self> {
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port))?;
        socket.set_read_timeout(Some(POLL))?;
        #[cfg(test)]
        let port = socket.local_addr()?.port();

        let load = Arc::new(Mutex::new(None::<Load>));
        let stop = Arc::new(AtomicBool::new(false));
        let id = node_id();
        let name = host_name();

        std::thread::Builder::new()
            .name("sysmo-reporter".into())
            .spawn({
                let (load, stop) = (load.clone(), stop.clone());
                move || {
                    let status = || {
                        let load = (*load.lock().unwrap())?;
                        Some(
                            Message::Status(Status {
                                id: id.clone(),
                                name: name.clone(),
                                cpu: load.cpu,
                                gpu: load.gpu,
                                gpus: load.gpus,
                            })
                            .encode(),
                        )
                    };
                    let mut watchers: HashMap<SocketAddr, Instant> = HashMap::new();
                    let mut next_send = Instant::now() + Duration::from_secs(1);
                    let mut buffer = [0u8; MAX_DATAGRAM];

                    while !stop.load(Ordering::Relaxed) {
                        if let Ok((length, from)) = socket.recv_from(&mut buffer) {
                            let message = std::str::from_utf8(&buffer[..length])
                                .ok()
                                .and_then(Message::decode);
                            match message {
                                Some(Message::Discover { lease }) if is_local(from.ip()) => {
                                    if watchers.len() < MAX_WATCHERS || watchers.contains_key(&from)
                                    {
                                        watchers
                                            .insert(from, Instant::now() + lease.min(MAX_LEASE));
                                        // Answer at once, so new nodes show up without waiting.
                                        if let Some(status) = status() {
                                            let _ = socket.send_to(status.as_bytes(), from);
                                        }
                                    }
                                }
                                Some(Message::Bye) => {
                                    watchers.remove(&from);
                                }
                                _ => {}
                            }
                        }

                        let now = Instant::now();
                        if now >= next_send {
                            next_send = now + Duration::from_secs(1);
                            watchers.retain(|_, until| *until > now);
                            if let Some(status) = status() {
                                for watcher in watchers.keys() {
                                    let _ = socket.send_to(status.as_bytes(), watcher);
                                }
                            }
                        }
                    }
                }
            })?;

        Ok(Self {
            #[cfg(test)]
            port,
            load,
            stop,
        })
    }

    #[cfg(test)]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The latest reading, sent with the next datagram.
    pub fn update(&self, cpu: f32, gpu: Option<f32>, gpus: u32) {
        *self.load.lock().unwrap() = Some(Load { cpu, gpu, gpus });
    }
}

impl Drop for Reporter {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// A status as received, with the address it came from.
pub struct Report {
    pub status: Status,
    pub from: IpAddr,
}

/// Asks the network who is there and passes on what the reporters send.
pub struct Watcher {
    socket: UdpSocket,
    target: SocketAddr,
    stop: Arc<AtomicBool>,
}

impl Watcher {
    /// `target` is where DISCOVER goes: the broadcast address on `PORT`.
    pub fn start(target: SocketAddr, reports: async_channel::Sender<Report>) -> io::Result<Self> {
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
        socket.set_broadcast(true)?;
        socket.set_read_timeout(Some(POLL))?;
        let receiving = socket.try_clone()?;
        let stop = Arc::new(AtomicBool::new(false));

        std::thread::Builder::new()
            .name("sysmo-watcher".into())
            .spawn({
                let stop = stop.clone();
                move || {
                    let mut buffer = [0u8; MAX_DATAGRAM];
                    while !stop.load(Ordering::Relaxed) {
                        let Ok((length, from)) = receiving.recv_from(&mut buffer) else {
                            continue;
                        };
                        let message = std::str::from_utf8(&buffer[..length])
                            .ok()
                            .and_then(Message::decode);
                        if let Some(Message::Status(status)) = message {
                            let report = Report {
                                status,
                                from: from.ip(),
                            };
                            if reports.send_blocking(report).is_err() {
                                break;
                            }
                        }
                    }
                }
            })?;

        Ok(Self {
            socket,
            target,
            stop,
        })
    }

    pub fn discover(&self) {
        let message = Message::Discover { lease: LEASE }.encode();
        let _ = self.socket.send_to(message.as_bytes(), self.target);
    }

    /// Tell the reporters at `nodes` to stop sending now rather than when the
    /// lease runs out.
    pub fn bye(&self, nodes: impl IntoIterator<Item = IpAddr>) {
        let message = Message::Bye.encode();
        for ip in nodes {
            let _ = self
                .socket
                .send_to(message.as_bytes(), (ip, self.target.port()));
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// A node's recent load, for the idle rule.
#[derive(Default)]
pub struct Activity {
    samples: VecDeque<(Instant, f32, Option<f32>)>,
}

impl Activity {
    pub fn push(&mut self, at: Instant, cpu: f32, gpu: Option<f32>) {
        self.samples.push_back((at, cpu, gpu));
        while let Some((first, ..)) = self.samples.front() {
            if at.duration_since(*first) > IDLE_WINDOW {
                self.samples.pop_front();
            } else {
                break;
            }
        }
    }

    /// Working when the CPU or the GPU averages at least
    /// `IDLE_BELOW_PERCENT` over the window; idle when both stay under it.
    pub fn busy(&self) -> bool {
        let average = |values: Vec<f32>| -> Option<f32> {
            (!values.is_empty()).then(|| values.iter().sum::<f32>() / values.len() as f32)
        };
        let cpu = average(self.samples.iter().map(|(_, cpu, _)| *cpu).collect());
        let gpu = average(self.samples.iter().filter_map(|(_, _, gpu)| *gpu).collect());
        cpu.is_some_and(|cpu| cpu >= IDLE_BELOW_PERCENT)
            || gpu.is_some_and(|gpu| gpu >= IDLE_BELOW_PERCENT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status() -> Status {
        Status {
            id: "00aa11bb22cc33dd".into(),
            name: "render 01".into(),
            cpu: 37.5,
            gpu: Some(88.0),
            gpus: 2,
        }
    }

    #[test]
    fn messages_round_trip() {
        for message in [
            Message::Discover {
                lease: Duration::from_secs(75),
            },
            Message::Bye,
            Message::Status(status()),
            Message::Status(Status {
                gpu: None,
                gpus: 0,
                ..status()
            }),
        ] {
            let text = message.encode();
            assert!(text.len() < 128, "{text}");
            assert_eq!(Message::decode(&text), Some(message));
        }
    }

    #[test]
    fn rejects_garbage() {
        for text in [
            "",
            "hello",
            "SYSMO/2 BYE",
            "SYSMO/1 DISCOVER",
            "SYSMO/1 STATUS id=x cpu=nan gpu=- gpus=0 name=a",
            "SYSMO/1 STATUS cpu=1 gpu=- gpus=0 name=a",
            "SYSMO/1 STATUS id=x cpu=1 gpu=- gpus=0",
        ] {
            assert_eq!(Message::decode(text), None, "{text}");
        }
        let clamped = Message::decode("SYSMO/1 STATUS id=x cpu=250 gpu=-3 gpus=1 name=a");
        let Some(Message::Status(status)) = clamped else {
            panic!("not decoded");
        };
        assert_eq!((status.cpu, status.gpu), (100.0, Some(0.0)));
    }

    #[test]
    fn directly_attached_networks_count_as_local() {
        let table =
            "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
            eth0\t00000000\t010200C0\t0003\t0\t0\t0\t00000000\t0\t0\t0\n\
            eth0\t000200C0\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0\n";
        assert!(on_link(table, "192.0.2.77".parse().unwrap()));
        // Reachable only through the gateway (the default route): not local.
        assert!(!on_link(table, "198.51.100.7".parse().unwrap()));
    }

    #[test]
    fn only_local_addresses_may_watch() {
        assert!(is_local("192.168.1.20".parse().unwrap()));
        assert!(is_local("10.0.0.5".parse().unwrap()));
        assert!(is_local("127.0.0.1".parse().unwrap()));
        assert!(!is_local("8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn idle_needs_both_cpu_and_gpu_quiet() {
        let start = Instant::now();
        let mut activity = Activity::default();
        for second in 0..30 {
            activity.push(start + Duration::from_secs(second), 3.0, Some(2.0));
        }
        assert!(!activity.busy());

        // A GPU at work is enough.
        let mut gpu_work = Activity::default();
        for second in 0..30 {
            gpu_work.push(start + Duration::from_secs(second), 2.0, Some(95.0));
        }
        assert!(gpu_work.busy());

        // No GPU: the CPU decides.
        let mut cpu_only = Activity::default();
        cpu_only.push(start, 40.0, None);
        assert!(cpu_only.busy());
    }

    #[test]
    fn a_short_pause_does_not_turn_idle() {
        let start = Instant::now();
        let mut activity = Activity::default();
        for second in 0..25 {
            activity.push(start + Duration::from_secs(second), 60.0, None);
        }
        for second in 25..30 {
            activity.push(start + Duration::from_secs(second), 0.0, None);
        }
        assert!(activity.busy());
        // Thirty quiet seconds later the work has left the window.
        for second in 30..61 {
            activity.push(start + Duration::from_secs(second), 0.0, None);
        }
        assert!(!activity.busy());
    }

    #[test]
    fn watcher_and_reporter_talk() {
        let reporter = Reporter::start(0).unwrap();
        reporter.update(42.0, Some(77.0), 1);
        let (sender, reports) = async_channel::unbounded();
        let target = SocketAddr::from((Ipv4Addr::LOCALHOST, reporter.port()));
        let watcher = Watcher::start(target, sender).unwrap();

        let receive = |within: Duration| -> Option<Report> {
            let until = Instant::now() + within;
            while Instant::now() < until {
                if let Ok(report) = reports.try_recv() {
                    return Some(report);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            None
        };

        assert!(
            receive(Duration::from_millis(500)).is_none(),
            "sent unasked"
        );
        watcher.discover();
        let first = receive(Duration::from_secs(1)).expect("no answer to DISCOVER");
        assert_eq!(first.status.cpu, 42.0);
        assert_eq!(first.status.gpu, Some(77.0));
        assert_eq!(first.status.id, node_id());
        // And then once a second.
        assert!(receive(Duration::from_millis(1600)).is_some());

        watcher.bye([IpAddr::V4(Ipv4Addr::LOCALHOST)]);
        std::thread::sleep(Duration::from_millis(400));
        while reports.try_recv().is_ok() {}
        assert!(
            receive(Duration::from_millis(1500)).is_none(),
            "kept sending after BYE"
        );
    }
}
