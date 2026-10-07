/* window.rs - the single SysMo window: a scrolling stack of sections fed by
 * the engine once per second and animated at ten frames per second.
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::time::Duration;

use adw::prelude::*;
use gtk::{gio, glib};
use gtk::glib::g_warning;

use crate::config;
use crate::dbus::{GaugeExport, GaugeState};
use crate::engine::{Engine, Gpu, Readings};
use crate::graph::{LoadGraph, FRAMES_PER_UNIT, UPDATE_INTERVAL_MS};
use crate::sections::{
    clean_cpu_name, CpuSection, DiskSection, GpuSection, MemorySection, NetworkSection,
};
use crate::units;

const KEY_WINDOW_WIDTH: &str = "window-width";
const KEY_WINDOW_HEIGHT: &str = "window-height";
const KEY_WINDOW_MAXIMIZED: &str = "window-maximized";
const KEY_SECTION_STATES: &str = "section-states";
const KEY_STAY_ON_TOP: &str = "stay-on-top";

pub struct SysmoWindow {
    window: adw::ApplicationWindow,
}

struct Inner {
    weak_self: Weak<Inner>,
    settings: Option<gio::Settings>,
    section_states: RefCell<HashMap<String, bool>>,
    sections_box: gtk::Box,
    cpu: CpuSection,
    memory: MemorySection,
    network: NetworkSection,
    disk: DiskSection,
    gpus: RefCell<Vec<GpuSection>>,
    graphs: RefCell<Vec<LoadGraph>>,
    engine: RefCell<Option<Engine>>,
    gauges: Option<GaugeExport>,
    frame: Cell<u32>,
    text_scaling: f64,
    /// Set by the "quit" action; otherwise closing the window only hides it
    /// so the top-bar gauges keep receiving data.
    quitting: Cell<bool>,
}

impl SysmoWindow {
    pub fn new(app: &adw::Application) -> Self {
        let settings = load_settings();
        let text_scaling = text_scaling_factor();
        let section_states = load_section_states(settings.as_ref());

        let (width, height, maximized) = match &settings {
            Some(s) => (
                s.int(KEY_WINDOW_WIDTH),
                s.int(KEY_WINDOW_HEIGHT),
                s.boolean(KEY_WINDOW_MAXIMIZED),
            ),
            None => (420, 740, false),
        };

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title(config::APP_NAME)
            .default_width(width.max(300))
            .default_height(height.max(300))
            .build();
        window.add_css_class("sysmo-root");
        if maximized {
            window.maximize();
        }

        let header = adw::HeaderBar::new();
        header.add_css_class("flat");
        header.set_title_widget(Some(&adw::WindowTitle::new(config::APP_NAME, "")));

        // GTK 4 cannot keep its own window on top (not at all on Wayland), so
        // this only stores the choice; the SysMo Gauges extension applies it.
        let stay_on_top = gtk::ToggleButton::builder()
            .icon_name("view-pin-symbolic")
            .tooltip_text("Stay on top")
            .build();
        if let Some(settings) = &settings {
            settings
                .bind(KEY_STAY_ON_TOP, &stay_on_top, "active")
                .build();
        }
        header.pack_start(&stay_on_top);

        let sections_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        sections_box.set_margin_top(4);
        sections_box.set_margin_bottom(8);
        sections_box.set_margin_start(12);
        sections_box.set_margin_end(12);

        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&sections_box)
            .build();

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&scroller));
        window.set_content(Some(&toolbar));

        let expanded = |id: &str| section_states.get(id).copied().unwrap_or(true);
        let cpu = CpuSection::new(expanded("cpu"), text_scaling);
        let memory = MemorySection::new(expanded("memory"), text_scaling);
        let network = NetworkSection::new(expanded("network"), text_scaling);
        let disk = DiskSection::new(expanded("disk"), text_scaling);

        for section in [
            &cpu.section,
            &memory.section,
            &network.section,
            &disk.section,
        ] {
            sections_box.append(&section.expander);
        }

        let graphs = vec![
            cpu.section.graph.clone(),
            memory.section.graph.clone(),
            network.section.graph.clone(),
            disk.section.graph.clone(),
        ];

        let (sender, receiver) = async_channel::unbounded::<Readings>();
        let engine = Engine::start(sender);
        let gauges = GaugeExport::new(app);

        let inner = Rc::new_cyclic(|weak| Inner {
            weak_self: weak.clone(),
            settings,
            section_states: RefCell::new(section_states),
            sections_box,
            cpu,
            memory,
            network,
            disk,
            gpus: RefCell::new(Vec::new()),
            graphs: RefCell::new(graphs),
            engine: RefCell::new(Some(engine)),
            gauges,
            frame: Cell::new(0),
            text_scaling,
            quitting: Cell::new(false),
        });

        for section in [
            &inner.cpu.section,
            &inner.memory.section,
            &inner.network.section,
            &inner.disk.section,
        ] {
            inner.track_expander(&section.id, &section.expander);
        }

        // Deliver samples from the engine thread to the widgets.
        glib::spawn_future_local({
            let inner = inner.clone();
            async move {
                while let Ok(readings) = receiver.recv().await {
                    inner.apply(&readings);
                }
            }
        });

        // Animation clock: one frame every 100 ms, one sample every ten frames.
        glib::timeout_add_local(Duration::from_millis(UPDATE_INTERVAL_MS), {
            let inner = Rc::downgrade(&inner);
            move || match inner.upgrade() {
                Some(inner) => {
                    inner.tick();
                    glib::ControlFlow::Continue
                }
                None => glib::ControlFlow::Break,
            }
        });

        // Closing the window hides it; SysMo keeps sampling in the background
        // for the top-bar gauges. Ctrl+Q (or the "quit" action) really exits.
        window.connect_close_request({
            let inner = inner.clone();
            move |window| {
                inner.save_window_state(window);
                if inner.quitting.get() {
                    // Stops the engine process before the window goes away.
                    inner.engine.borrow_mut().take();
                    glib::Propagation::Proceed
                } else {
                    window.set_visible(false);
                    glib::Propagation::Stop
                }
            }
        });

        let quit = gio::SimpleAction::new("quit", None);
        quit.connect_activate({
            let inner = inner.clone();
            let window = window.clone();
            move |_, _| {
                inner.quitting.set(true);
                window.close();
            }
        });
        app.add_action(&quit);
        app.set_accels_for_action("app.quit", &["<Control>q"]);
        app.set_accels_for_action("window.close", &["<Control>w"]);

        Self { window }
    }

    pub fn present(&self) {
        self.window.present();
    }
}

impl Inner {
    fn tick(&self) {
        for graph in self.graphs.borrow().iter() {
            graph.tick();
        }

        let frame = self.frame.get().wrapping_add(1);
        self.frame.set(frame);
        if frame % FRAMES_PER_UNIT == 0 {
            if let Some(engine) = self.engine.borrow().as_ref() {
                engine.request_sample();
            }
        }
    }

    fn apply(&self, readings: &Readings) {
        self.cpu.update(&readings.cpu);
        if let Some(devices) = &readings.memory_devices {
            self.memory.set_devices(devices, readings.memory.mem_total);
        }
        self.memory.update(&readings.memory);
        self.network.update(&readings.connections);
        self.disk.update(&readings.disks);
        self.sync_gpus(&readings.gpus);
        if let Some(gauges) = &self.gauges {
            gauges.publish(Self::gauge_state(readings));
        }
    }

    /// GPUs shown in the window: integrated Intel graphics are hidden
    /// whenever a discrete GPU exists.
    fn displayed_gpus(gpus: &HashMap<String, Gpu>) -> Vec<&Gpu> {
        let has_discrete = gpus.values().any(|gpu| !GpuSection::is_integrated(gpu));
        let mut ordered: Vec<&Gpu> = gpus
            .values()
            .filter(|gpu| !(has_discrete && GpuSection::is_integrated(gpu)))
            .collect();
        ordered.sort_by_key(|gpu| gpu.id.clone());
        ordered
    }

    /// The five top-bar figures plus the text the gauges show on hover. With
    /// several GPUs, the GPU gauges show the average of all of them.
    fn gauge_state(readings: &Readings) -> GaugeState {
        let mut state = GaugeState::default();
        let cpu = &readings.cpu;

        state
            .values
            .insert("cpu-load".to_owned(), cpu.total_usage_percent as f64);
        if let Some(celsius) = cpu.temperature_celsius {
            state.values.insert("cpu-temp".to_owned(), celsius as f64);
        }
        if let Some(name) = cpu.name.as_deref() {
            let name = clean_cpu_name(name);
            if !name.is_empty() {
                state.names.insert("cpu".to_owned(), name);
            }
        }
        state.details.insert(
            "cpu-load".to_owned(),
            format!("{} cores", cpu.core_usage_percent.len()),
        );

        let gpus = Self::displayed_gpus(&readings.gpus);
        if let Some(name) = Self::gpu_names(&gpus) {
            state.names.insert("gpu".to_owned(), name);
        }
        let load = gpus
            .iter()
            .filter_map(|gpu| gpu.utilization_percent.map(f64::from));
        if let Some(percent) = average(load) {
            state.values.insert("gpu-load".to_owned(), percent);
        }
        let vram = gpus
            .iter()
            .filter_map(|gpu| match (gpu.used_memory, gpu.total_memory) {
                (Some(used), Some(total)) if total > 0 => Some(used as f64 * 100.0 / total as f64),
                _ => None,
            });
        if let Some(percent) = average(vram) {
            state.values.insert("gpu-vram".to_owned(), percent);
        }
        let temperature = gpus
            .iter()
            .filter_map(|gpu| gpu.temperature_c.map(f64::from));
        if let Some(celsius) = average(temperature) {
            state.values.insert("gpu-temp".to_owned(), celsius);
        }

        match gpus.as_slice() {
            [gpu] => {
                if let (Some(used), Some(total)) = (gpu.used_memory, gpu.total_memory) {
                    if total > 0 {
                        state.details.insert(
                            "gpu-vram".to_owned(),
                            format!("{} of {}", units::bytes_si(used), units::bytes_si(total)),
                        );
                    }
                }
            }
            [] => {}
            _ => {
                for key in ["gpu-load", "gpu-vram", "gpu-temp"] {
                    state.details.insert(key.to_owned(), "average".to_owned());
                }
            }
        }

        state
    }

    /// "RTX 4080", "2 × RTX 4080" or "RTX 4080 + RTX 3060".
    fn gpu_names(gpus: &[&Gpu]) -> Option<String> {
        let names: Vec<String> = gpus
            .iter()
            .map(|gpu| GpuSection::device_name(gpu))
            .collect();
        let first = names.first()?;
        if names.len() > 1 && names.iter().all(|name| name == first) {
            Some(format!("{} × {first}", names.len()))
        } else {
            Some(names.join(" + "))
        }
    }

    fn sync_gpus(&self, gpus: &HashMap<String, Gpu>) {
        for gpu in Self::displayed_gpus(gpus) {
            let id = GpuSection::section_id(gpu);
            let known = self
                .gpus
                .borrow()
                .iter()
                .position(|section| section.section.id == id);

            match known {
                Some(index) => self.gpus.borrow()[index].update(gpu),
                None => {
                    let expanded = self
                        .section_states
                        .borrow()
                        .get(&id)
                        .copied()
                        .unwrap_or(true);
                    let section = GpuSection::new(gpu, expanded, self.text_scaling);
                    self.sections_box.append(&section.section.expander);
                    self.track_expander(&section.section.id, &section.section.expander);
                    self.graphs.borrow_mut().push(section.section.graph.clone());
                    section.update(gpu);
                    self.gpus.borrow_mut().push(section);
                }
            }
        }
    }

    fn track_expander(&self, id: &str, expander: &gtk::Expander) {
        let inner = self.weak_self.clone();
        let id = id.to_owned();
        expander.connect_expanded_notify(move |expander| {
            if let Some(inner) = inner.upgrade() {
                inner
                    .section_states
                    .borrow_mut()
                    .insert(id.clone(), expander.is_expanded());
                inner.save_section_states();
            }
        });
    }

    fn save_section_states(&self) {
        let Some(settings) = &self.settings else {
            return;
        };
        let states = self.section_states.borrow();
        let mut entries: Vec<String> = states
            .iter()
            .map(|(id, expanded)| {
                format!("{id}:{}", if *expanded { "expanded" } else { "collapsed" })
            })
            .collect();
        entries.sort();
        let entries: Vec<&str> = entries.iter().map(String::as_str).collect();
        if let Err(e) = settings.set_strv(KEY_SECTION_STATES, entries.as_slice()) {
            g_warning!("SysMo", "Failed to save section states: {}", e);
        }
    }

    fn save_window_state(&self, window: &adw::ApplicationWindow) {
        let Some(settings) = &self.settings else {
            return;
        };
        let maximized = window.is_maximized();
        let _ = settings.set_boolean(KEY_WINDOW_MAXIMIZED, maximized);
        if !maximized {
            let (width, height) = window.default_size();
            if width > 0 && height > 0 {
                let _ = settings.set_int(KEY_WINDOW_WIDTH, width);
                let _ = settings.set_int(KEY_WINDOW_HEIGHT, height);
            }
        }
    }
}

/// Mean of the readings that are present; `None` when there are none.
fn average(values: impl Iterator<Item = f64>) -> Option<f64> {
    let (sum, count) = values.fold((0.0, 0u32), |(sum, count), value| (sum + value, count + 1));
    (count > 0).then(|| sum / count as f64)
}

fn load_settings() -> Option<gio::Settings> {
    let source = gio::SettingsSchemaSource::default()?;
    if source.lookup(config::APP_ID, true).is_none() {
        g_warning!(
            "SysMo",
            "GSettings schema {} is not installed; window state will not be remembered",
            config::APP_ID
        );
        return None;
    }
    Some(gio::Settings::new(config::APP_ID))
}

fn load_section_states(settings: Option<&gio::Settings>) -> HashMap<String, bool> {
    let mut states = HashMap::new();
    let Some(settings) = settings else {
        return states;
    };
    for entry in settings.strv(KEY_SECTION_STATES).iter() {
        let entry = entry.as_str();
        if let Some((id, state)) = entry.rsplit_once(':') {
            match state {
                "expanded" => {
                    states.insert(id.to_owned(), true);
                }
                "collapsed" => {
                    states.insert(id.to_owned(), false);
                }
                _ => {}
            }
        }
    }
    states
}

/// GNOME's "Large Text" accessibility setting, mirrored by System Monitor
/// for its graph captions.
fn text_scaling_factor() -> f64 {
    const SCHEMA: &str = "org.gnome.desktop.interface";
    let Some(source) = gio::SettingsSchemaSource::default() else {
        return 1.0;
    };
    if source.lookup(SCHEMA, true).is_none() {
        return 1.0;
    }
    let factor = gio::Settings::new(SCHEMA).double("text-scaling-factor");
    if factor > 0.0 {
        factor
    } else {
        1.0
    }
}
