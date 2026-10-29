/* ssh_nodes.rs - nodes watched over SSH, for computers that do not run
 * SysMo.
 *
 * The watching SysMo logs in with the user's own SSH setup (keys, agent,
 * ~/.ssh/config) and runs a small read-only loop on the node that prints
 * the CPU counters and the NVIDIA GPU load once a second. Nothing is
 * installed on the node. One connection per node, open only while the
 * Network tab is on screen.
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

use std::io::{BufRead, BufReader, Read};
use std::net::{IpAddr, Ipv4Addr};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::lan::{Report, Status};

/// The most addresses one range may name.
const MAX_TARGETS: usize = 1024;
/// A node that does not answer is tried again after this long...
const RETRY_OFFLINE: Duration = Duration::from_secs(60);
/// ...one that refuses the login after this long, so its logs do not fill
/// up with failed attempts.
const RETRY_NO_ACCESS: Duration = Duration::from_secs(600);

/// Runs on the node, under `sh` whatever the login shell is, using only
/// sh, grep, head and nvidia-smi. Prints "H <hostname>" once, then once a
/// second, in a single write so each node costs one small packet:
///   G <util> <util> ...          one value per NVIDIA GPU (when there are any)
///   C <first line of /proc/stat>
/// One long-running nvidia-smi paces the loop, which is far cheaper than
/// starting it every second. Every second's write also notices a closed
/// connection, so nothing lingers on the node afterwards.
const REMOTE_SCRIPT: &str = "exec sh -c 'exec 2>/dev/null; \
    echo \"H $(hostname)\"; \
    n=$(nvidia-smi -L | grep -c \"^GPU\"); \
    if [ \"${n:-0}\" -gt 0 ]; then \
      nvidia-smi --query-gpu=utilization.gpu --format=csv,noheader,nounits -l 1 | { \
        i=0; g=; \
        while read -r u; do \
          g=\"$g $u\"; i=$((i + 1)); \
          if [ \"$i\" -ge \"$n\" ]; then \
            printf \"G%s\\nC %s\\n\" \"$g\" \"$(head -n 1 /proc/stat)\"; i=0; g=; \
          fi; \
        done; }; \
    else \
      while :; do echo \"C $(head -n 1 /proc/stat)\"; sleep 1; done; \
    fi'";

pub enum Event {
    Status(Report),
    /// The node is up but will not let us in.
    NoAccess {
        ip: IpAddr,
        reason: String,
    },
}

/// Parse "192.168.1.100-130", "192.168.1.10-192.168.1.20",
/// "192.168.1.0/24", single addresses, or a mix separated by commas or
/// spaces.
pub fn parse_targets(text: &str) -> Result<Vec<Ipv4Addr>, String> {
    let mut targets: Vec<Ipv4Addr> = Vec::new();
    for token in text
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|token| !token.is_empty())
    {
        let bad = || format!("“{token}” is not an address, range or subnet");
        let (first, last) = if let Some((base, bits)) = token.split_once('/') {
            let base: Ipv4Addr = base.parse().map_err(|_| bad())?;
            let bits: u32 = bits.parse().map_err(|_| bad())?;
            if !(22..=32).contains(&bits) {
                return Err(format!("“{token}”: use a subnet from /22 to /32"));
            }
            let mask = u32::MAX.checked_shl(32 - bits).unwrap_or(0);
            let network = u32::from(base) & mask;
            let broadcast = network | !mask;
            if bits >= 31 {
                (network, broadcast)
            } else {
                (network + 1, broadcast - 1)
            }
        } else if let Some((start, end)) = token.split_once('-') {
            let start: Ipv4Addr = start.parse().map_err(|_| bad())?;
            let end: Ipv4Addr = match end.parse::<u8>() {
                Ok(last_octet) => {
                    let [a, b, c, _] = start.octets();
                    Ipv4Addr::new(a, b, c, last_octet)
                }
                Err(_) => end.parse().map_err(|_| bad())?,
            };
            if end < start {
                return Err(format!("“{token}”: the range runs backwards"));
            }
            (u32::from(start), u32::from(end))
        } else {
            let address: Ipv4Addr = token.parse().map_err(|_| bad())?;
            (u32::from(address), u32::from(address))
        };
        if (last - first) as usize >= MAX_TARGETS {
            return Err(format!("“{token}” names more than {MAX_TARGETS} addresses"));
        }
        for address in first..=last {
            let address = Ipv4Addr::from(address);
            if !targets.contains(&address) {
                targets.push(address);
            }
        }
        if targets.len() > MAX_TARGETS {
            return Err(format!("More than {MAX_TARGETS} addresses"));
        }
    }
    Ok(targets)
}

/// CPU busy and total time from a /proc/stat "cpu" line.
fn cpu_times(line: &str) -> Option<(u64, u64)> {
    let mut fields = line.split_whitespace();
    if fields.next()? != "cpu" {
        return None;
    }
    // user nice system idle iowait irq softirq steal (guest time is
    // already counted in user).
    let values: Vec<u64> = fields
        .take(8)
        .map(|v| v.parse().ok())
        .collect::<Option<_>>()?;
    if values.len() < 4 {
        return None;
    }
    let total: u64 = values.iter().sum();
    let idle = values[3] + values.get(4).copied().unwrap_or(0);
    Some((total - idle, total))
}

fn cpu_percent(before: (u64, u64), after: (u64, u64)) -> Option<f32> {
    let busy = after.0.checked_sub(before.0)?;
    let total = after.1.checked_sub(before.1)?;
    (total > 0).then(|| (busy as f32 * 100.0 / total as f32).clamp(0.0, 100.0))
}

/// "87 45" -> [87.0, 45.0]; anything unreadable ("[N/A]") is left out.
fn gpu_values(text: &str) -> Vec<f32> {
    text.split_whitespace()
        .filter_map(|value| value.parse::<f32>().ok())
        .filter(|value| value.is_finite())
        .map(|value| value.clamp(0.0, 100.0))
        .collect()
}

enum Outcome {
    /// Did not answer: switched off, unplugged, not there.
    Offline,
    NoAccess(String),
    /// Was connected, then the connection ended.
    Ended,
}

/// Why ssh failed, from what it printed.
fn classify(stderr: &str) -> Outcome {
    let stderr = stderr.to_lowercase();
    if stderr.contains("permission denied") {
        Outcome::NoAccess("SSH key not accepted".to_owned())
    } else if stderr.contains("host key verification failed") {
        Outcome::NoAccess("Unknown host key: connect once with ssh from a terminal".to_owned())
    } else if stderr.contains("connection refused") {
        Outcome::NoAccess("No SSH server".to_owned())
    } else if stderr.contains("could not resolve") || stderr.contains("bad configuration") {
        Outcome::NoAccess(stderr.lines().next().unwrap_or_default().trim().to_owned())
    } else {
        Outcome::Offline
    }
}

type ChildSlot = Arc<Mutex<Option<Child>>>;

/// One SSH session: report every second until it ends.
fn session(
    ip: Ipv4Addr,
    user: Option<&str>,
    child_slot: &ChildSlot,
    events: &async_channel::Sender<Event>,
) -> Outcome {
    let mut command = Command::new("ssh");
    command
        .args(["-T", "-o", "BatchMode=yes", "-o", "ConnectTimeout=5"])
        .args(["-o", "ServerAliveInterval=5", "-o", "ServerAliveCountMax=2"])
        // Our own connection, whatever multiplexing ~/.ssh/config sets up.
        .args(["-o", "ControlMaster=no", "-o", "ControlPath=none"]);
    if let Some(user) = user {
        command.args(["-l", user]);
    }
    command
        .arg(ip.to_string())
        .arg(REMOTE_SCRIPT)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => return Outcome::NoAccess(format!("Can't run ssh: {e}")),
    };
    let stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    *child_slot.lock().unwrap() = Some(child);

    let id = format!("ssh-{ip}");
    let mut name = ip.to_string();
    let mut previous: Option<(u64, u64)> = None;
    let mut gpus: Vec<f32> = Vec::new();
    let mut connected = false;

    for line in stdout
        .into_iter()
        .flat_map(|out| BufReader::new(out).lines())
    {
        let Ok(line) = line else { break };
        connected = true;
        if let Some(host) = line.strip_prefix("H ") {
            name = host.trim().chars().take(64).collect();
        } else if let Some(values) = line.strip_prefix('G') {
            gpus = gpu_values(values);
        } else if let Some(times) = line.strip_prefix("C ").and_then(cpu_times) {
            let cpu = previous.and_then(|before| cpu_percent(before, times));
            previous = Some(times);
            let Some(cpu) = cpu else { continue };
            let gpu = (!gpus.is_empty()).then(|| gpus.iter().sum::<f32>() / gpus.len() as f32);
            let report = Report {
                status: Status {
                    id: id.clone(),
                    name: name.clone(),
                    cpu,
                    gpu,
                    gpus: gpus.len() as u32,
                },
                from: IpAddr::V4(ip),
            };
            if events.send_blocking(Event::Status(report)).is_err() {
                break;
            }
        }
    }

    let mut message = String::new();
    if let Some(stderr) = stderr.as_mut() {
        let _ = stderr.read_to_string(&mut message);
    }
    if let Some(mut child) = child_slot.lock().unwrap().take() {
        let _ = child.kill();
        let _ = child.wait();
    }
    if connected {
        Outcome::Ended
    } else {
        classify(&message)
    }
}

/// Watches a list of addresses over SSH, one thread and one connection per
/// address. Dropping it closes every connection.
pub struct SshMonitor {
    stop: Arc<AtomicBool>,
    children: Vec<ChildSlot>,
}

impl SshMonitor {
    pub fn start(
        targets: Vec<Ipv4Addr>,
        user: Option<String>,
        events: async_channel::Sender<Event>,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let mut children = Vec::new();
        for ip in targets {
            let child_slot: ChildSlot = Arc::new(Mutex::new(None));
            children.push(child_slot.clone());
            let (stop, user, events) = (stop.clone(), user.clone(), events.clone());
            let spawned = std::thread::Builder::new()
                .name(format!("sysmo-ssh-{ip}"))
                .spawn(move || {
                    let mut next_try = Instant::now();
                    while !stop.load(Ordering::Relaxed) {
                        if Instant::now() < next_try {
                            std::thread::sleep(Duration::from_millis(250));
                            continue;
                        }
                        let outcome = session(ip, user.as_deref(), &child_slot, &events);
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        next_try = Instant::now()
                            + match outcome {
                                Outcome::NoAccess(reason) => {
                                    let event = Event::NoAccess {
                                        ip: IpAddr::V4(ip),
                                        reason,
                                    };
                                    if events.send_blocking(event).is_err() {
                                        break;
                                    }
                                    RETRY_NO_ACCESS
                                }
                                Outcome::Offline | Outcome::Ended => RETRY_OFFLINE,
                            };
                    }
                });
            if spawned.is_err() {
                break;
            }
        }
        Self { stop, children }
    }
}

impl Drop for SshMonitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for slot in &self.children {
            if let Some(child) = slot.lock().unwrap().as_mut() {
                let _ = child.kill();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(text: &str) -> Ipv4Addr {
        text.parse().unwrap()
    }

    #[test]
    fn parses_ranges_subnets_and_lists() {
        let short = parse_targets("192.168.1.100-130").unwrap();
        assert_eq!(short.len(), 31);
        assert_eq!(short[0], ip("192.168.1.100"));
        assert_eq!(short[30], ip("192.168.1.130"));

        let long = parse_targets("10.0.0.250-10.0.1.5").unwrap();
        assert_eq!(long.len(), 12);

        // Network and broadcast addresses left out.
        let subnet = parse_targets("192.168.1.0/24").unwrap();
        assert_eq!(subnet.len(), 254);
        assert_eq!(subnet[0], ip("192.168.1.1"));

        let mixed = parse_targets("192.168.1.5, 192.168.1.7 192.168.1.5").unwrap();
        assert_eq!(mixed, vec![ip("192.168.1.5"), ip("192.168.1.7")]);
        assert!(parse_targets("  ").unwrap().is_empty());
    }

    #[test]
    fn rejects_bad_ranges() {
        for text in [
            "192.168.1",
            "192.168.1.130-100",
            "192.168.1.1-300",
            "10.0.0.0/8",
            "hello",
            "10.0.0.0-10.0.8.0",
        ] {
            assert!(parse_targets(text).is_err(), "{text}");
        }
    }

    #[test]
    fn cpu_load_from_proc_stat() {
        let before = cpu_times("cpu  100 0 100 800 0 0 0 0 0 0").unwrap();
        let after = cpu_times("cpu  250 0 150 1400 0 0 0 0 0 0").unwrap();
        // 200 busy of 800 ticks.
        assert_eq!(cpu_percent(before, after), Some(25.0));
        // iowait counts as idle.
        let waiting = cpu_times("cpu  250 0 150 1400 600 0 0 0 0 0").unwrap();
        assert_eq!(cpu_percent(after, waiting), Some(0.0));
        assert!(cpu_times("cpu0 1 2 3 4").is_none());
        assert!(cpu_times("intr 1 2 3").is_none());
    }

    #[test]
    fn gpu_lines() {
        assert_eq!(gpu_values(" 87 45"), vec![87.0, 45.0]);
        assert_eq!(gpu_values(" 87 [N/A]"), vec![87.0]);
        assert!(gpu_values("").is_empty());
    }

    #[test]
    fn remote_script_runs_under_sh() {
        // The exact command ssh hands to the node's login shell.
        let mut child = Command::new("sh")
            .args(["-c", REMOTE_SCRIPT])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let first = lines.next().unwrap().unwrap();
        assert!(first.starts_with("H "), "{first}");
        let mut cpu_lines = 0;
        for line in lines.by_ref() {
            let line = line.unwrap();
            if let Some(stat) = line.strip_prefix("C ") {
                assert!(cpu_times(stat).is_some(), "{line}");
                cpu_lines += 1;
                if cpu_lines == 2 {
                    break;
                }
            } else {
                assert!(line.starts_with("G "), "{line}");
                assert!(!gpu_values(&line[1..]).is_empty(), "{line}");
            }
        }
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn failures_are_told_apart() {
        let reason = |stderr| match classify(stderr) {
            Outcome::NoAccess(reason) => Some(reason),
            _ => None,
        };
        assert!(reason("user@10.0.0.5: Permission denied (publickey).").is_some());
        assert!(reason("Host key verification failed.").is_some());
        assert!(reason("ssh: connect to host 10.0.0.5 port 22: Connection refused").is_some());
        assert!(reason("ssh: connect to host 10.0.0.5 port 22: Connection timed out").is_none());
        assert!(reason("ssh: connect to host 10.0.0.5 port 22: No route to host").is_none());
    }
}
