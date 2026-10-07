/* sections.rs - the collapsible CPU / Memory / Network / Disk / GPU blocks,
 * each a System Monitor style graph with a single compact legend line.
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

use std::cell::Cell;
use std::collections::HashMap;

use gtk::{gdk, prelude::*};

use crate::engine::{Connection, Cpu, Disk, Gpu, Memory, MemoryDevice};
use crate::graph::{LoadGraph, Scale, Series};
use crate::units;

/// GNOME System Monitor's default palette (org.gnome.gnome-system-monitor).
pub const CPU_COLORS: [&str; 16] = [
    "#e01b24", "#ff7800", "#f6d32d", "#33d17a", "#26a269", "#62a0ea", "#1c71d8", "#613583",
    "#9141ac", "#c061cb", "#ffbe6f", "#f9f06b", "#8ff0a4", "#2ec27e", "#1a5fb4", "#c061cb",
];
pub const MEMORY_COLOR: &str = "#e01b24";
pub const SWAP_COLOR: &str = "#33d17a";
pub const NET_IN_COLOR: &str = "#3584e4";
pub const NET_OUT_COLOR: &str = "#e66100";
pub const DISK_READ_COLOR: &str = "#3584e4";
pub const DISK_WRITE_COLOR: &str = "#e66100";
/// System Monitor has no GPU graph; a vivid green for load, so it stands out,
/// and the GNOME palette's yellow for VRAM.
pub const GPU_LOAD_COLOR: &str = "#00e676";
pub const GPU_VRAM_COLOR: &str = "#f6d32d";
/// Temperatures share the 0-100 axis (1 % = 1 °C) and are drawn as a dashed
/// white curve, which stands out against every colour in the palette.
pub const TEMPERATURE_COLOR: &str = "#ffffff";

/// CPU and GPU loads above this are shown red on white.
const HIGH_LOAD_PERCENT: f32 = 80.0;

const PCI_VENDOR_INTEL: u32 = 0x8086;
const PCI_VENDOR_NVIDIA: u32 = 0x10de;
const PCI_VENDOR_AMD: u32 = 0x1002;
const NOT_AVAILABLE: &str = "n/a";

fn vendor_name(vendor_id: u32) -> &'static str {
    match vendor_id {
        PCI_VENDOR_INTEL => "Intel",
        PCI_VENDOR_NVIDIA => "NVIDIA",
        PCI_VENDOR_AMD => "AMD",
        _ => "Unknown vendor",
    }
}

pub fn rgba(hex: &str) -> gdk::RGBA {
    gdk::RGBA::parse(hex).unwrap_or(gdk::RGBA::WHITE)
}

fn temperature_series() -> Series {
    Series::dashed(rgba(TEMPERATURE_COLOR))
}

fn temperature_fraction(celsius: Option<f32>) -> f64 {
    celsius.map(|c| c as f64 / 100.0).unwrap_or(-1.0)
}

fn temperature_text(celsius: Option<f32>) -> String {
    match celsius {
        Some(c) => units::temperature(c),
        None => NOT_AVAILABLE.to_owned(),
    }
}

fn mark_high_load(label: &gtk::Label, percent: Option<f32>) {
    if percent.is_some_and(|p| p > HIGH_LOAD_PERCENT) {
        label.add_css_class("high-load");
    } else {
        label.remove_css_class("high-load");
    }
}

/// Small colour swatch for the legend: a rounded rectangle for solid series,
/// a dashed stroke for dashed ones.
fn color_chip(series: Series) -> gtk::DrawingArea {
    let chip = gtk::DrawingArea::new();
    chip.set_content_width(16);
    chip.set_content_height(10);
    chip.set_valign(gtk::Align::Center);
    chip.set_draw_func(move |_, cr, width, height| {
        let (w, h) = (width as f64, height as f64);
        let c = series.color;
        cr.set_source_rgba(
            c.red() as f64,
            c.green() as f64,
            c.blue() as f64,
            c.alpha() as f64,
        );
        if series.dashed {
            cr.set_line_width(2.0);
            cr.set_dash(&[4.0, 2.0], 0.0);
            cr.move_to(0.0, h / 2.0);
            cr.line_to(w, h / 2.0);
            let _ = cr.stroke();
        } else {
            let r = 3.0;
            cr.new_sub_path();
            cr.arc(w - r, r, r, -std::f64::consts::FRAC_PI_2, 0.0);
            cr.arc(w - r, h - r, r, 0.0, std::f64::consts::FRAC_PI_2);
            cr.arc(r, h - r, r, std::f64::consts::FRAC_PI_2, std::f64::consts::PI);
            cr.arc(r, r, r, std::f64::consts::PI, 1.5 * std::f64::consts::PI);
            cr.close_path();
            let _ = cr.fill();
        }
    });
    chip
}

/// A titled, collapsible block holding one graph and a legend row.
pub struct Section {
    pub id: String,
    pub expander: gtk::Expander,
    pub graph: LoadGraph,
    title: gtk::Label,
    legend: gtk::Box,
}

impl Section {
    fn new(id: &str, title: &str, graph: LoadGraph, expanded: bool) -> Self {
        let title_label = gtk::Label::new(Some(title));
        title_label.add_css_class("section-title");
        title_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title_label.set_xalign(0.0);

        let expander = gtk::Expander::new(None);
        expander.set_label_widget(Some(&title_label));

        let content = gtk::Box::new(gtk::Orientation::Vertical, 4);
        content.set_margin_top(6);
        content.set_vexpand(true);
        content.append(graph.widget());

        let legend = gtk::Box::new(gtk::Orientation::Horizontal, 18);
        legend.add_css_class("legend");
        legend.set_margin_start(4);
        legend.set_margin_end(4);
        legend.set_margin_bottom(2);
        content.append(&legend);

        expander.set_child(Some(&content));
        expander.set_expanded(expanded);
        expander.set_vexpand(expanded);
        expander
            .bind_property("expanded", &expander, "vexpand")
            .build();

        Self {
            id: id.to_owned(),
            expander,
            graph,
            title: title_label,
            legend,
        }
    }

    pub fn set_title(&self, title: &str) {
        if self.title.text() != title {
            self.title.set_text(title);
        }
    }

    /// Append "[chip] name value" to the legend and return the value label.
    fn add_legend_item(&self, series: Option<Series>, name: &str, value_width: i32) -> gtk::Label {
        let item = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        if let Some(series) = series {
            item.append(&color_chip(series));
        }

        let name_label = gtk::Label::new(Some(name));
        name_label.add_css_class("dim");
        item.append(&name_label);

        let value = gtk::Label::new(None);
        value.add_css_class("value");
        value.add_css_class("tnum");
        value.set_xalign(0.0);
        if value_width > 0 {
            value.set_width_chars(value_width);
        }
        item.append(&value);

        self.legend.append(&item);
        value
    }
}

/// "Intel(R) Core(TM) Ultra 5 235T" -> "Intel Core Ultra 5 235T".
pub fn clean_cpu_name(raw: &str) -> String {
    let base = raw.split('@').next().unwrap_or(raw);
    base.replace("(R)", "")
        .replace("(TM)", "")
        .replace("(tm)", "")
        .replace('®', "")
        .replace('™', "")
        .split_whitespace()
        .filter(|word| *word != "CPU" && *word != "Processor")
        .collect::<Vec<_>>()
        .join(" ")
}

/// Disk models come out of sysfs in shouting case ("SAMSUNG ..."); tidy the
/// vendor word for the brands where that is unambiguous.
fn pretty_model(raw: &str) -> String {
    const VENDORS: [(&str, &str); 10] = [
        ("SAMSUNG", "Samsung"),
        ("KINGSTON", "Kingston"),
        ("SEAGATE", "Seagate"),
        ("TOSHIBA", "Toshiba"),
        ("INTEL", "Intel"),
        ("CRUCIAL", "Crucial"),
        ("MICRON", "Micron"),
        ("HITACHI", "Hitachi"),
        ("SANDISK", "SanDisk"),
        ("CORSAIR", "Corsair"),
    ];
    let mut words: Vec<String> = raw.split_whitespace().map(str::to_owned).collect();
    if let Some(first) = words.first_mut() {
        if let Some((_, pretty)) = VENDORS.iter().find(|(upper, _)| upper == first) {
            *first = (*pretty).to_owned();
        }
    }
    words.join(" ")
}

pub struct CpuSection {
    pub section: Section,
    usage: gtk::Label,
    temperature: gtk::Label,
    cores: Cell<usize>,
}

impl CpuSection {
    pub fn new(expanded: bool, text_scaling: f64) -> Self {
        let graph = LoadGraph::new(Scale::Percent, Vec::new(), text_scaling);
        let section = Section::new("cpu", "CPU", graph, expanded);
        let usage = section.add_legend_item(None, "Load", 6);
        let temperature = section.add_legend_item(Some(temperature_series()), "Temp", 6);
        Self {
            section,
            usage,
            temperature,
            cores: Cell::new(0),
        }
    }

    pub fn update(&self, cpu: &Cpu) {
        let cores = cpu.core_usage_percent.len();
        if cores != self.cores.get() {
            self.cores.set(cores);
            // One curve per core in System Monitor's colours, plus the
            // dashed temperature curve on top.
            let mut series: Vec<Series> = (0..cores)
                .map(|i| Series::solid(rgba(CPU_COLORS[i % CPU_COLORS.len()])))
                .collect();
            series.push(temperature_series());
            self.section.graph.set_series(series);
        }

        let mut fractions: Vec<f64> = cpu
            .core_usage_percent
            .iter()
            .map(|percent| *percent as f64 / 100.0)
            .collect();
        fractions.push(temperature_fraction(cpu.temperature_celsius));
        self.section.graph.push_fractions(&fractions);

        self.usage
            .set_text(&format!("{:.1}%", cpu.total_usage_percent));
        mark_high_load(&self.usage, Some(cpu.total_usage_percent));
        self.temperature
            .set_text(&temperature_text(cpu.temperature_celsius));

        if let Some(name) = cpu.name.as_deref() {
            let name = clean_cpu_name(name);
            if !name.is_empty() {
                self.section.set_title(&format!("CPU · {name}"));
            }
        }
    }
}

pub struct MemorySection {
    pub section: Section,
    memory: gtk::Label,
    swap: gtk::Label,
}

impl MemorySection {
    pub fn new(expanded: bool, text_scaling: f64) -> Self {
        let graph = LoadGraph::new(
            Scale::Percent,
            vec![
                Series::solid(rgba(MEMORY_COLOR)),
                Series::solid(rgba(SWAP_COLOR)),
            ],
            text_scaling,
        );
        let section = Section::new("memory", "Memory", graph, expanded);
        let memory =
            section.add_legend_item(Some(Series::solid(rgba(MEMORY_COLOR))), "Memory", 0);
        let swap = section.add_legend_item(Some(Series::solid(rgba(SWAP_COLOR))), "Swap", 0);
        Self {
            section,
            memory,
            swap,
        }
    }

    /// Title from the installed modules: "Memory · 32 GB DDR5 5200 MT/s".
    pub fn set_devices(&self, devices: &[MemoryDevice], mem_total: u64) {
        let module_bytes: u64 = devices.iter().map(|device| device.size).sum();
        let mut title = if module_bytes > 0 {
            let gib = (module_bytes as f64 / 1_073_741_824.0).round();
            format!("Memory · {gib:.0} GB")
        } else {
            format!("Memory · {}", units::bytes_si(mem_total))
        };

        if let Some(ram_type) = devices
            .iter()
            .map(|device| device.ram_type.trim())
            .find(|ram_type| !ram_type.is_empty() && !ram_type.eq_ignore_ascii_case("unknown"))
        {
            title.push(' ');
            title.push_str(ram_type);
        }
        if let Some(speed) = devices
            .iter()
            .map(|device| device.speed)
            .filter(|speed| *speed > 0)
            .max()
        {
            title.push_str(&format!(" {speed} MT/s"));
        }

        self.section.set_title(&title);
    }

    pub fn update(&self, info: &Memory) {
        let total = info.mem_total;
        let used = total.saturating_sub(info.mem_available);
        let fraction = if total > 0 {
            used as f64 / total as f64
        } else {
            0.0
        };

        let swap_total = info.swap_total;
        let swap_used = swap_total.saturating_sub(info.swap_free);
        let swap_fraction = if swap_total > 0 {
            swap_used as f64 / swap_total as f64
        } else {
            -1.0
        };

        self.section
            .graph
            .push_fractions(&[fraction, swap_fraction]);

        self.memory.set_text(&format!(
            "{} ({})",
            units::bytes_si(used),
            units::percent(fraction)
        ));
        self.memory
            .set_tooltip_text(Some(&format!("of {}", units::bytes_si(total))));
        if swap_total > 0 {
            self.swap.set_text(&format!(
                "{} ({})",
                units::bytes_si(swap_used),
                units::percent(swap_fraction)
            ));
            self.swap
                .set_tooltip_text(Some(&format!("of {}", units::bytes_si(swap_total))));
        } else {
            self.swap.set_text("not available");
            self.swap.set_tooltip_text(None);
        }
    }
}

pub struct NetworkSection {
    pub section: Section,
    receiving: gtk::Label,
    sending: gtk::Label,
}

impl NetworkSection {
    pub fn new(expanded: bool, text_scaling: f64) -> Self {
        let graph = LoadGraph::new(
            Scale::Rate,
            vec![
                Series::solid(rgba(NET_IN_COLOR)),
                Series::solid(rgba(NET_OUT_COLOR)),
            ],
            text_scaling,
        );
        let section = Section::new("network", "Network", graph, expanded);
        let receiving =
            section.add_legend_item(Some(Series::solid(rgba(NET_IN_COLOR))), "Receiving", 10);
        let sending =
            section.add_legend_item(Some(Series::solid(rgba(NET_OUT_COLOR))), "Sending", 10);
        Self {
            section,
            receiving,
            sending,
        }
    }

    pub fn update(&self, connections: &HashMap<String, Connection>) {
        use magpie_types::network::ConnectionKind;

        let (mut incoming, mut outgoing) = (0.0f64, 0.0f64);
        for connection in connections.values() {
            // Bridges and container/VM adapters carry traffic that the
            // physical adapter already accounts for.
            if matches!(
                connection.kind(),
                ConnectionKind::Bridge
                    | ConnectionKind::Docker
                    | ConnectionKind::Multipass
                    | ConnectionKind::Virtual
            ) {
                continue;
            }
            incoming += connection.rx_rate_bytes_ps.max(0.0) as f64;
            outgoing += connection.tx_rate_bytes_ps.max(0.0) as f64;
        }

        let (incoming, outgoing) = (incoming as u64, outgoing as u64);
        self.section.graph.push_rates(incoming, outgoing);
        self.receiving.set_text(&units::rate(incoming));
        self.sending.set_text(&units::rate(outgoing));
    }
}

pub struct DiskSection {
    pub section: Section,
    reading: gtk::Label,
    writing: gtk::Label,
}

impl DiskSection {
    pub fn new(expanded: bool, text_scaling: f64) -> Self {
        let graph = LoadGraph::new(
            Scale::Rate,
            vec![
                Series::solid(rgba(DISK_READ_COLOR)),
                Series::solid(rgba(DISK_WRITE_COLOR)),
            ],
            text_scaling,
        );
        let section = Section::new("disk", "Disk", graph, expanded);
        let reading =
            section.add_legend_item(Some(Series::solid(rgba(DISK_READ_COLOR))), "Reading", 10);
        let writing =
            section.add_legend_item(Some(Series::solid(rgba(DISK_WRITE_COLOR))), "Writing", 10);
        Self {
            section,
            reading,
            writing,
        }
    }

    pub fn update(&self, disks: &[Disk]) {
        let read: u64 = disks.iter().map(|disk| disk.rx_speed_bytes_ps).sum();
        let written: u64 = disks.iter().map(|disk| disk.tx_speed_bytes_ps).sum();
        self.section.graph.push_rates(read, written);
        self.reading.set_text(&units::rate(read));
        self.writing.set_text(&units::rate(written));

        // Title: the system disk's model, plus a count of any further disks
        // (the graph sums all of them).
        let mut named: Vec<&Disk> = disks
            .iter()
            .filter(|disk| disk.capacity_bytes > 0)
            .filter(|disk| disk.model.as_deref().is_some_and(|m| !m.trim().is_empty()))
            .collect();
        named.sort_by_key(|disk| (!disk.is_system, disk.id.clone()));
        if let Some(first) = named.first() {
            let model = pretty_model(first.model.as_deref().unwrap_or_default());
            let title = if named.len() > 1 {
                format!("Disk · {model} +{}", named.len() - 1)
            } else {
                format!("Disk · {model}")
            };
            self.section.set_title(&title);
        }
    }
}

pub struct GpuSection {
    pub section: Section,
    load: gtk::Label,
    vram: gtk::Label,
    temperature: gtk::Label,
}

impl GpuSection {
    pub fn section_id(gpu: &Gpu) -> String {
        format!("gpu:{}", gpu.id)
    }

    pub fn is_integrated(gpu: &Gpu) -> bool {
        gpu.vendor_id == PCI_VENDOR_INTEL
    }

    /// Marketing name, or the vendor when the engine has no name for it.
    pub fn device_name(gpu: &Gpu) -> String {
        gpu.device_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| vendor_name(gpu.vendor_id).to_owned())
    }

    pub fn title(gpu: &Gpu) -> String {
        format!("GPU · {}", Self::device_name(gpu))
    }

    pub fn new(gpu: &Gpu, expanded: bool, text_scaling: f64) -> Self {
        let graph = LoadGraph::new(
            Scale::Percent,
            vec![
                Series::solid(rgba(GPU_LOAD_COLOR)),
                Series::solid(rgba(GPU_VRAM_COLOR)),
                temperature_series(),
            ],
            text_scaling,
        );
        let section = Section::new(&Self::section_id(gpu), &Self::title(gpu), graph, expanded);
        let load = section.add_legend_item(Some(Series::solid(rgba(GPU_LOAD_COLOR))), "Load", 6);
        let vram = section.add_legend_item(Some(Series::solid(rgba(GPU_VRAM_COLOR))), "VRAM", 0);
        let temperature = section.add_legend_item(Some(temperature_series()), "Temp", 6);
        Self {
            section,
            load,
            vram,
            temperature,
        }
    }

    pub fn update(&self, gpu: &Gpu) {
        let load = gpu
            .utilization_percent
            .map(|percent| percent as f64 / 100.0)
            .unwrap_or(-1.0);
        let vram = match (gpu.used_memory, gpu.total_memory) {
            (Some(used), Some(total)) if total > 0 => used as f64 / total as f64,
            _ => -1.0,
        };
        self.section
            .graph
            .push_fractions(&[load, vram, temperature_fraction(gpu.temperature_c)]);

        self.load.set_text(&match gpu.utilization_percent {
            Some(percent) => format!("{percent:.1}%"),
            None => NOT_AVAILABLE.to_owned(),
        });
        mark_high_load(&self.load, gpu.utilization_percent);
        match (gpu.used_memory, gpu.total_memory) {
            (Some(used), Some(total)) if total > 0 => {
                self.vram.set_text(&format!(
                    "{} ({})",
                    units::bytes_si(used),
                    units::percent(used as f64 / total as f64)
                ));
                self.vram
                    .set_tooltip_text(Some(&format!("of {}", units::bytes_si(total))));
            }
            _ => {
                self.vram.set_text(NOT_AVAILABLE);
                self.vram.set_tooltip_text(None);
            }
        }
        self.temperature
            .set_text(&temperature_text(gpu.temperature_c));
        self.section.set_title(&Self::title(gpu));
    }
}
