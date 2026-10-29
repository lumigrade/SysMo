# SysMo

A compact system monitor for GNOME that looks like the stock **System
Monitor → Resources** tab, drawn on dark glass, and adds what that tab is
missing: **CPU temperature, GPU load, VRAM and GPU temperature** — plus five
live ring gauges in the top bar.

![The SysMo window](screenshots/window.png)

## Why this exists

GNOME System Monitor has the right look and the right update rate, but it
cannot show a CPU temperature and knows nothing about your GPU. Mission
Center knows all of that, but its interface is a Windows Task Manager
lookalike. SysMo keeps Mission Center's sensor engine, throws its interface
away, and rebuilds the one page worth having.

## What you get

* **CPU** — one curve per core in System Monitor's colours, plus the package
  temperature as a dashed white curve on the same graph (1 % = 1 °C)
* **Memory and swap**
* **Network** — receive and send, auto-scaling axis
* **Disk** — read and write, summed across drives
* **GPU** — utilisation, VRAM use and temperature for every discrete GPU. An
  integrated Intel GPU is hidden automatically when a discrete card exists.
* Section titles name your actual hardware: CPU model, memory size, type and
  speed, disk model, GPU model
* Graphs sample once a second and scroll at ten frames a second, exactly like
  GNOME System Monitor. Every section collapses and remembers its state.
* Closing the window keeps SysMo running for the top-bar gauges;
  <kbd>Ctrl</kbd>+<kbd>Q</kbd> quits properly.
* **DeepCool AK700 DIGITAL** (and coolers sharing its display protocol) —
  SysMo feeds the cooler's display and can hold each fan at its own fixed
  speed; see [DeepCool cooler and fans](#deepcool-cooler-and-fans)
* **Network tab** — every SysMo on your local network with its live CPU and
  GPU load, green while it works and red while it idles; see
  [Network tab](#network-tab)

### Top bar gauges

![Gauges in the top bar](screenshots/panel.png)

Five ring gauges: CPU load, CPU temperature, GPU load, VRAM and GPU
temperature. Each ring fills clockwise from 12 o'clock (a full turn is 100)
and its colour follows the reading along the arc — cool blue at rest, green
from 45, yellow from 65, red from 80. Each gauge also keeps an identity
colour in its faint track ring.

Behind the number a glossy disc fades in from 55, blends through amber and
orange to red by 80 and deepens towards 100, with no hard steps. Temperatures
use the same numbers in °C. With more than one GPU, the three GPU gauges show
the average across the GPUs.

![Gauges under load](screenshots/panel-load.png)

Hovering a gauge enlarges it and names the reading; clicking one opens or
raises the SysMo window.

![Hovering a gauge](screenshots/hover.png)

## How SysMo was made

SysMo is a **fork of [Mission Center][mc]** (upstream `main`, commit
`fd060f17`, September 2026). Mission Center's engine was kept and its entire
user interface was deleted and rewritten — about 20,000 of 23,000 front-end
lines are gone.

The code SysMo inherits comes from **five upstream repositories**:

| Repository | What it provides | Licence | How it is included |
|---|---|---|---|
| [magpie][magpie] | The sensor engine: every reading SysMo shows. A separate process, spoken to over an nng socket with Protocol Buffers. | GPL-3.0-or-later | git submodule, built unmodified as `sysmo-magpie` |
| [app-rummage][rummage] | Application detection; a build dependency of magpie. | MIT | nested submodule inside magpie |
| [NVTOP][nvtop] | The GPU backend — utilisation, VRAM, temperature, power. | GPL-3.0 | downloaded, checksummed and patched by magpie's build script |
| [GNOME System Monitor][gsm] | The graph rendering. `src/graph.rs` is a Rust and cairo port of its `load-graph.cpp` and `gsm-graph.c`, reproducing the grid, time axis, smooth scrolling and axis rounding. | GPL-2.0-or-later | ported source, not copied |
| [deepcool-digital-linux][deepcool] | The USB packet for DeepCool cooler displays (AK700 / LQ series mapping table), used by `src/cooler.rs`. | GPL-3.0 | protocol ported, not copied |

[CREDITS.md](CREDITS.md) has the exact commits, what was kept, what was
removed, and the licence reasoning.

## Requirements

* **Ubuntu 24.04 or newer**, or any distribution with GTK 4.14 and
  libadwaita 1.5. Mission Center's own interface needs GTK 4.22; SysMo
  deliberately does not, so it builds on current Ubuntu.
* **GNOME 45 or newer** for the top-bar gauges (tested on GNOME 46, X11 and
  Wayland). The window itself works on any desktop.
* **Rust 1.90 or newer.** Ubuntu's `rustc` package is 1.75 and too old, so
  install Rust with rustup as shown below.
* An internet connection for the first build (it fetches NVTOP and the Rust
  crates).

## Installation

These steps work on a freshly installed Ubuntu. Copy them one block at a
time.

### 1. Install the build dependencies

```bash
sudo apt update
sudo apt install -y build-essential cmake curl desktop-file-utils git \
    libadwaita-1-dev libdrm-dev libgbm-dev libudev-dev \
    meson ninja-build patch pkg-config python3
```

### 2. Install Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
source "$HOME/.cargo/env"
```

### 3. Get the source

The engine is a submodule, so clone recursively. A plain `git clone` will not
build.

```bash
git clone --recurse-submodules https://github.com/lumigrade/SysMo.git
cd SysMo
```

Already cloned without submodules? Run `git submodule update --init --recursive`.

### 4. Build and install

```bash
meson setup build --prefix="$HOME/.local" -Dbuildtype=release
ninja -C build install
```

Everything is installed for your user only; no `sudo`, nothing outside your
home directory:

| Path | What |
|---|---|
| `~/.local/bin/sysmo` | the application |
| `~/.local/bin/sysmo-magpie` | the sensor engine |
| `~/.local/share/sysmo/hw.db` | hardware name database |
| `~/.local/share/applications/` | app launcher entry |
| `~/.local/share/glib-2.0/schemas/` | settings schema |
| `~/.local/share/gnome-shell/extensions/sysmo-gauges@sysmo.org/` | the top-bar gauges |
| `~/.config/autostart/` | starts SysMo at login |

**The first build takes several minutes** — it compiles the engine and NVTOP
from source. Later builds take seconds.

### 5. Turn on the top-bar gauges

GNOME Shell only looks for new extensions when it starts, so restart it
once and then enable the extension:

* **On X11:** press <kbd>Alt</kbd>+<kbd>F2</kbd>, type `r`, press Enter.
* **On Wayland:** log out and log back in.

```bash
gnome-extensions enable sysmo-gauges@sysmo.org
```

Check it worked:

```bash
gnome-extensions info sysmo-gauges@sysmo.org
```

It should say `State: ACTIVE`. The gauges appear as soon as SysMo is running.

### 6. Start it

```bash
sysmo
```

or launch **SysMo** from the app grid. From the next login it starts on its
own. To stop that, either pass `-Dautostart=false` when configuring the build
or delete `~/.config/autostart/org.sysmo.SysMo.desktop`.

If the `sysmo` command is not found, `~/.local/bin` is not on your `PATH`;
log out and back in, since Ubuntu adds it at login when the directory exists.

## Using it

| Action | Result |
|---|---|
| Click the pin button | keep the window on top of others (needs the top-bar extension); remembered across restarts |
| Click a section title | collapse or expand it; remembered across restarts |
| Close the window | window hides, gauges keep updating |
| <kbd>Ctrl</kbd>+<kbd>Q</kbd> | quit for real, engine included |
| Click a top-bar gauge | open or raise the window |
| Hover a top-bar gauge | reading, device name and detail |
| Click the cooler button at the right of the CPU title | open the DeepCool cooler panel (only shown when such a cooler is plugged in) |

## Network tab

The title bar switches between **This PC** (everything above) and
**Network**: a list of every computer on your local network that runs
SysMo, with its CPU and GPU load updated every second.

* **Green, Working** — its CPU or its GPU has averaged 10 % or more over the
  last 30 seconds.
* **Red, Idle** — both have stayed under 10 % for 30 seconds.
* **Grey, No reply** — nothing heard for 5 seconds. After 60 seconds of
  silence the node is removed from the list.

New nodes show up within a minute, or at once with the ↻ button. Each node
needs SysMo installed and running (it starts at login, and keeps reporting
with its window closed). **Share this PC's load**, at the bottom of the tab,
turns a computer's reporting off.

**Light on the network.** Nothing scans the network. While the tab is open,
SysMo broadcasts one small "who's there?" a minute; each SysMo answers and
then sends one datagram of about 100 bytes a second, to the watching SysMo
only — about 5 KB/s for 50 nodes. Leave the tab (or close the window) and
every node stops within a second; nothing is sent while nobody watches. Only
computers on the local network are answered.

### Nodes without SysMo (over SSH)

Computers you do not want to install SysMo on can be watched over SSH
instead, as long as this PC can already log in to them with a key (try
`ssh 192.168.1.101`: no password prompt). Under **Nodes without SysMo** enter
their addresses, for example `192.168.1.100-130`, `192.168.1.0/24` or a list
like `192.168.1.5, 192.168.1.9`, and press ✓. An optional **SSH user** is
used for all of them; leave it blank to use `~/.ssh/config` or your own user
name.

While the Network tab is open, SysMo keeps one SSH connection to each
address and runs a small read-only loop there that prints the CPU counters
(`/proc/stat`) and the NVIDIA GPU load (`nvidia-smi`) once a second. Nothing
is installed or changed on the node, and the loop ends as soon as the
connection closes. That costs about 10 KB to log in and then about 200
bytes a second per node — roughly 6 KB/s for 30 nodes.

* A node that does not answer is not listed and is tried again every
  60 seconds, so switching it on makes it appear within a minute.
* A node that answers but refuses the login shows a grey **No access** row
  with the reason, and is tried again every 10 minutes (not every minute, so
  its logs do not fill up with failed logins). "Unknown host key" means this
  PC has never connected to it: run `ssh <address>` once from a terminal and
  accept the key.
* A computer that runs SysMo and is also in the list appears only once.

**Firewall.** SysMo uses UDP port 47917. Ubuntu's firewall is off by
default; if you turned it on, open the port on every node:

```bash
sudo ufw allow 47917/udp
```

Nodes must be on the same network segment as the watching PC (the
broadcast does not cross routers).

## DeepCool cooler and fans

With a **DeepCool AK700 DIGITAL** plugged in over USB, a small `AK700 ▾`
button appears at the right end of the CPU title. It opens a panel with:

* **Display** — SysMo sends CPU power, temperature, load and clock speed to
  the cooler's display once a second, in °C or °F. No other program is
  needed. The AK620 G2, AK500 G2 and AK400 G2 DIGITAL, LQ240, LQ360 and
  ASSASSIN IV VC VISION use the same protocol and are recognised too.
* **Fans** — the cooler's fans are plugged into the motherboard, which sets
  their speed. Every fan that spins gets a button (`Fan 1`, `Fan 2`, …) next
  to the "Fans" heading; clicking one shows that fan's own live RPM,
  **Fixed speed** switch and speed, each remembered separately. With Fixed
  speed off the motherboard stays in charge of that fan; switched on, the fan
  is held at 30–100 %. Fixed fans go to full speed if the CPU reaches 85 °C,
  and every fan is handed back to the motherboard when SysMo quits.

### One-time setup

Linux only lets root talk to the cooler and the fan controller, so both need
a permission rule once. From the SysMo source folder:

```bash
sudo cp data/70-sysmo.rules /etc/udev/rules.d/
sudo udevadm control --reload
sudo udevadm trigger --action=add --subsystem-match=hidraw --subsystem-match=hwmon
```

**Fan control also needs the motherboard's sensor driver.** Many boards
(including the ASUS ROG STRIX B860-I) do not load it automatically. Try it,
then check that a fan control appeared:

```bash
sudo modprobe nct6775
ls /sys/class/hwmon/*/pwm*_enable
```

If files are listed, load the driver at every boot and apply the rule to it:

```bash
echo nct6775 | sudo tee /etc/modules-load.d/sysmo-fans.conf
sudo udevadm trigger --action=add --subsystem-match=hwmon
```

If nothing is listed, `sudo dmesg | grep -i nct` usually says why; an "ACPI
resource conflict" means the board firmware reserves the chip.

**Optional: CPU power on the display.** The power figure shows 0 W unless
the CPU energy counter is readable. Linux hides it on purpose (a power side
channel, CVE-2020-8694), so this step is your call:

```bash
sudo cp data/71-sysmo-cpu-power.rules /etc/udev/rules.d/
sudo udevadm control --reload
sudo udevadm trigger --action=add --subsystem-match=powercap
```

If the panel says "Needs the one-time setup", a rule is missing or has not
been applied yet; a reboot applies all of them.

## Uninstalling

```bash
ninja -C build uninstall
gnome-extensions disable sysmo-gauges@sysmo.org
```

## Troubleshooting

**The gauges are not in the top bar.** Restart GNOME Shell (step 5) and
confirm `gnome-extensions info sysmo-gauges@sysmo.org` says `ACTIVE`. Errors
show up in `journalctl --user -u org.gnome.Shell@x11.service -f`.

**Gauges show `–` or say "SysMo is not running".** Start SysMo; the gauges
read from it over D-Bus and hold no sensors of their own.

**CPU temperature shows `n/a`.** The engine reads `coretemp`, `k10temp` or
`zenpower` under `/sys/class/hwmon`. Check with
`for h in /sys/class/hwmon/hwmon*; do cat $h/name; done`.

**GPU section missing or empty.** NVIDIA cards need the proprietary driver
(NVML). Confirm with `nvidia-smi`.

**Build fails while fetching NVTOP.** That step needs network access; rerun
`ninja -C build install` once you are online.

**Memory says just the size, without type or speed.** Memory module details
come from DMI data exposed by udev. Some firmware does not publish it.

## For developers

```bash
cargo build --release --manifest-path subprojects/magpie/Cargo.toml   # engine
cargo build --release                                                # front end
SYSMO_DATA_DIR=build/src target/release/sysmo                        # run it
```

| Path | What |
|---|---|
| [`src/main.rs`](src/main.rs) | entry point, signal handling |
| [`src/window.rs`](src/window.rs) | the window, the 100 ms clock, saved state |
| [`src/sections.rs`](src/sections.rs) | the five sections and their legends |
| [`src/graph.rs`](src/graph.rs) | the GNOME System Monitor graph port |
| [`src/engine/`](src/engine) | starts the engine, samples it over nng |
| [`src/dbus.rs`](src/dbus.rs) | publishes the gauge figures |
| [`src/cooler.rs`](src/cooler.rs) | DeepCool cooler display over USB HID |
| [`src/fans.rs`](src/fans.rs) | motherboard fan headers (hwmon PWM) |
| [`src/cooler_panel.rs`](src/cooler_panel.rs) | the cooler panel in the CPU section |
| [`src/lan.rs`](src/lan.rs) | Network tab protocol: reporter, watcher, idle rule |
| [`src/network_page.rs`](src/network_page.rs) | the Network tab |
| [`src/ssh_nodes.rs`](src/ssh_nodes.rs) | Network tab nodes read over SSH |
| [`extension/`](extension) | the GNOME Shell extension |

Environment variables: `SYSMO_ENGINE` (engine binary), `SYSMO_DATA_DIR`
(directory holding `hw.db`), `SYSMO_ENGINE_SOCKET` (attach to a running
engine), `G_MESSAGES_DEBUG=SysMo` for logging.

The gauges read `org.sysmo.SysMo` at `/org/sysmo/SysMo/Gauges`, interface
`org.sysmo.SysMo.Gauges`, properties `Values` (`a{sd}`), `Names` and
`Details` (`a{ss}`), with `PropertiesChanged` on every sample:

```bash
gdbus call --session --dest org.sysmo.SysMo \
  --object-path /org/sysmo/SysMo/Gauges \
  --method org.freedesktop.DBus.Properties.GetAll org.sysmo.SysMo.Gauges
```

After editing the extension, reinstall and restart GNOME Shell; stylesheet
changes also need the extension disabled and enabled again.

## Licence

GPL-3.0-or-later — see [COPYING](COPYING). SysMo inherits code from Mission
Center, magpie, app-rummage, NVTOP, GNOME System Monitor and
deepcool-digital-linux; their authors
and licences are credited in [CREDITS.md](CREDITS.md).

[mc]: https://gitlab.com/mission-center-devs/mission-center
[magpie]: https://gitlab.com/mission-center-devs/gng
[rummage]: https://gitlab.com/mission-center-devs/app-detection
[nvtop]: https://github.com/Syllo/nvtop
[gsm]: https://gitlab.gnome.org/GNOME/gnome-system-monitor
[deepcool]: https://github.com/Nortank12/deepcool-digital-linux
