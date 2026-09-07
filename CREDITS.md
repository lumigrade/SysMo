# Where SysMo's code comes from

SysMo is not written from scratch. It is a **fork of [Mission Center][mc]**
that keeps Mission Center's data engine and replaces its entire user
interface. This file records exactly what was inherited, from where, and
under which licence.

## The fork

| | |
|---|---|
| Forked from | [Mission Center][mc] (`mission-center-devs/mission-center`) |
| Fork point | upstream `main`, commit `fd060f17`, 6 September 2026 (version string 1.2.0) |
| Licence | GPL-3.0-or-later |
| Kept | the data engine and the process that talks to it |
| Removed | the whole GTK front end: performance page, apps page, services page, process table, preferences and dialogs, plus 45 translations, the Flatpak and Snap packaging and the `graph-widget` subproject |
| Added | a single GNOME System Monitor style window, a GNOME Shell extension, and a small D-Bus interface between them |

Roughly 20,000 of Mission Center's 23,000 front-end lines were deleted. What
remains of Mission Center in SysMo is the engine (unmodified, as a submodule)
and a trimmed copy of its IPC client in
[`src/engine/client.rs`](src/engine/client.rs), which still carries its
original copyright header.

## The four repositories SysMo inherits from

### 1. magpie — the data engine

* Source: <https://gitlab.com/mission-center-devs/gng>
* Pinned at: `e797fe5a2717f42c58484fce400458cf53efbfea`
* Licence: GPL-3.0-or-later
* In this repo: git submodule at `subprojects/magpie`, **built unmodified**
  and installed as `sysmo-magpie`

magpie is Mission Center's sensor daemon. It runs as a separate process,
reads the CPU, memory, disk, network and GPU counters, and answers requests
over an nng socket using Protocol Buffers. Every number SysMo displays comes
from it. SysMo uses five of its requests (CPU, memory, memory devices, disks,
network connections, GPUs) and ignores the rest.

### 2. app-rummage — application detection

* Source: <https://gitlab.com/mission-center-devs/app-detection>
* Pinned at: `26b93b8c6f0de6912f0a17022f7ece76bad87612`
* Licence: MIT
* In this repo: nested submodule inside magpie, at
  `subprojects/magpie/platform-linux/crates/app-rummage`

A dependency of magpie that maps running processes to installed
applications. SysMo does not show a process list, but magpie does not build
without it.

### 3. NVTOP — the GPU backend

* Source: <https://github.com/Syllo/nvtop>
* Pinned at: `3d4a953da02bc18886734613bb9f60ff80669de7`
* Licence: GPL-3.0
* In this repo: **not vendored.** magpie's build script downloads the tarball,
  verifies its SHA-256, applies eleven patches carried in
  `subprojects/magpie/platform-linux/3rdparty/nvtop/patches/`, and compiles it
  into the engine.

NVTOP is where GPU utilisation, VRAM, temperature, clocks and power come
from. On this machine it reaches the NVIDIA card through NVML; it also
supports AMD, Intel and several ARM GPUs. This is the reason the first build
needs a network connection and takes a while.

### 4. GNOME System Monitor — the graphs

* Source: <https://gitlab.gnome.org/GNOME/gnome-system-monitor> (version 46.0)
* Licence: GPL-2.0-or-later (`Files: *` in the Debian/Ubuntu copyright file;
  `src/gsm-graph.c` additionally carries an LGPL-2-or-later header)
* In this repo: **ported, not copied.**
  [`src/graph.rs`](src/graph.rs) is a Rust and cairo re-implementation of
  `src/load-graph.cpp` and `src/gsm-graph.c`

This is what makes SysMo look like the real thing rather than an imitation.
The port reproduces the original's geometry and behaviour: the 4 px frame,
the seven vertical time sections, the 18 px indent and six-character right
margin, the grid alpha values (0.7 for the border, half that for the inner
lines), the label offset factors, the bar-count thresholds, the ten
animation frames per sample that make the curve scroll smoothly, and the
"nice number" rounding that rescales the network and disk axes. The default
colours are read from GNOME System Monitor's own GSettings palette.

GPL-2-or-later is upward compatible with GPL-3, so this port can be
distributed as part of a GPL-3-or-later work.

## Licence of the result

SysMo as a whole is **GPL-3.0-or-later**; the full text is in
[COPYING](COPYING). Files written for SysMo carry an
`SPDX-License-Identifier: GPL-3.0-or-later` header. Inherited files keep the
copyright notices of their original authors.

[mc]: https://gitlab.com/mission-center-devs/mission-center
