/* cooler_panel.rs - the drop-down behind the cooler button in the CPU
 * section's title: what the DeepCool cooler's display shows, and the speed
 * of each fan, every fan with its own settings.
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::io;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::gio;

use crate::cooler::{self, Link, Reading};
use crate::engine::Cpu;
use crate::fans::{self, Control, Header};

const KEY_DISPLAY: &str = "cooler-display";
const KEY_FAHRENHEIT: &str = "cooler-fahrenheit";
const KEY_FAN_PROFILES: &str = "fan-profiles";
const KEY_FAN_AUTO_MODES: &str = "fan-auto-modes";
const KEY_FAN_SELECTED: &str = "fan-selected";

const MIN_PERCENT: u8 = 30;
const DEFAULT_PERCENT: u8 = 60;

/// Fans held at a fixed speed run at full speed from this temperature until
/// the CPU is back under `COOL_CELSIUS`.
const HOT_CELSIUS: f32 = 85.0;
const COOL_CELSIUS: f32 = 80.0;

const SETUP_HINT: &str = "Needs the one-time setup (see README)";

/// One fan's own settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Profile {
    fixed: bool,
    percent: u8,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            fixed: false,
            percent: DEFAULT_PERCENT,
        }
    }
}

#[derive(Clone)]
pub struct CoolerPanel(Rc<Inner>);

struct Inner {
    weak_self: Weak<Inner>,
    settings: Option<gio::Settings>,
    display: cooler::Display,
    button: gtk::ToggleButton,
    button_label: gtk::Label,
    revealer: gtk::Revealer,
    display_group: adw::PreferencesGroup,
    show_row: adw::SwitchRow,
    fahrenheit_row: adw::SwitchRow,
    /// One toggle button per fan, at the right of the "Fans" heading.
    fan_buttons: gtk::Box,
    fan_row: adw::ActionRow,
    fixed_row: adw::SwitchRow,
    speed_row: adw::SpinRow,
    missing_row: adw::ActionRow,
    /// Every header whose speed can be set.
    headers: RefCell<Vec<Header>>,
    /// Headers seen spinning this session; they keep their button even if
    /// the fan stops for a while.
    seen_spinning: RefCell<HashSet<String>>,
    /// Ids of the headers that have a button, in order.
    shown: RefCell<Vec<String>>,
    selected: RefCell<Option<String>>,
    profiles: RefCell<HashMap<String, Profile>>,
    controls: RefCell<HashMap<String, Control>>,
    errors: RefCell<HashMap<String, String>>,
    hot: Cell<bool>,
    /// Set while the rows are filled with another fan's settings.
    loading: Cell<bool>,
}

impl CoolerPanel {
    pub fn new(settings: Option<gio::Settings>) -> Self {
        let button_label = gtk::Label::new(Some("Cooler"));
        let arrow = gtk::Image::from_icon_name("pan-down-symbolic");
        let button_content = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        button_content.append(&button_label);
        button_content.append(&arrow);
        let button = gtk::ToggleButton::builder()
            .child(&button_content)
            .tooltip_text("Cooler display and fans")
            .valign(gtk::Align::Center)
            .visible(false)
            .build();
        button.add_css_class("flat");
        button.add_css_class("cooler-button");
        button.connect_toggled(move |button| {
            arrow.set_icon_name(Some(if button.is_active() {
                "pan-up-symbolic"
            } else {
                "pan-down-symbolic"
            }));
        });

        let show_row = adw::SwitchRow::builder()
            .title("Show readings")
            .active(true)
            .build();
        let fahrenheit_row = adw::SwitchRow::builder().title("Fahrenheit").build();
        let display_group = adw::PreferencesGroup::builder().title("Display").build();
        display_group.add(&show_row);
        display_group.add(&fahrenheit_row);

        let fan_buttons = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        fan_buttons.add_css_class("linked");
        fan_buttons.set_valign(gtk::Align::Center);
        let fan_row = adw::ActionRow::new();
        let fixed_row = adw::SwitchRow::builder().title("Fixed speed").build();
        let speed_row = adw::SpinRow::with_range(MIN_PERCENT as f64, 100.0, 5.0);
        speed_row.set_title("Speed (%)");
        speed_row.set_subtitle("Full speed above 85 °C");
        speed_row.set_value(DEFAULT_PERCENT as f64);
        fixed_row
            .bind_property("active", &speed_row, "sensitive")
            .sync_create()
            .build();
        let missing_row = adw::ActionRow::new();
        let fan_group = adw::PreferencesGroup::builder().title("Fans").build();
        fan_group.set_header_suffix(Some(&fan_buttons));
        fan_group.add(&fan_row);
        fan_group.add(&fixed_row);
        fan_group.add(&speed_row);
        fan_group.add(&missing_row);

        let column = gtk::Box::new(gtk::Orientation::Vertical, 12);
        column.set_margin_top(4);
        column.set_margin_bottom(8);
        column.append(&display_group);
        column.append(&fan_group);

        let revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideDown)
            .child(&column)
            .build();
        button
            .bind_property("active", &revealer, "reveal-child")
            .sync_create()
            .build();

        let mut profiles = HashMap::new();
        let mut selected = None;
        if let Some(settings) = &settings {
            settings.bind(KEY_DISPLAY, &show_row, "active").build();
            settings
                .bind(KEY_FAHRENHEIT, &fahrenheit_row, "active")
                .build();
            profiles = load_profiles(settings);
            selected =
                Some(settings.string(KEY_FAN_SELECTED).to_string()).filter(|id| !id.is_empty());
        }

        let panel = Self(Rc::new_cyclic(|weak| Inner {
            weak_self: weak.clone(),
            settings,
            display: cooler::Display::start(),
            button,
            button_label,
            revealer,
            display_group,
            show_row,
            fahrenheit_row,
            fan_buttons,
            fan_row,
            fixed_row,
            speed_row,
            missing_row,
            headers: RefCell::new(Vec::new()),
            seen_spinning: RefCell::new(HashSet::new()),
            shown: RefCell::new(Vec::new()),
            selected: RefCell::new(selected),
            profiles: RefCell::new(profiles),
            controls: RefCell::new(HashMap::new()),
            errors: RefCell::new(HashMap::new()),
            hot: Cell::new(false),
            loading: Cell::new(false),
        }));

        let weak = Rc::downgrade(&panel.0);
        panel.0.fixed_row.connect_active_notify({
            let weak = weak.clone();
            move |row| {
                if let Some(inner) = weak.upgrade() {
                    let fixed = row.is_active();
                    inner.edit_profile(|profile| profile.fixed = fixed);
                }
            }
        });
        panel.0.speed_row.connect_value_notify(move |row| {
            if let Some(inner) = weak.upgrade() {
                let percent = row.value().round() as u8;
                inner.edit_profile(|profile| profile.percent = percent);
            }
        });

        panel.0.refresh_fans();
        panel
    }

    /// The button that opens the panel, for the CPU title row.
    pub fn button(&self) -> &gtk::ToggleButton {
        &self.0.button
    }

    /// The panel itself, for above the CPU graph.
    pub fn widget(&self) -> &gtk::Revealer {
        &self.0.revealer
    }

    /// Called with every sample.
    pub fn update(&self, cpu: &Cpu) {
        self.0.update_display(cpu);
        if let Some(celsius) = cpu.temperature_celsius {
            if celsius >= HOT_CELSIUS {
                self.0.hot.set(true);
            } else if celsius < COOL_CELSIUS {
                self.0.hot.set(false);
            }
        }
        self.0.refresh_fans();
        self.0.apply_fans();
        self.0.show_fan_state();
    }

    /// Hand every fan back to the motherboard (on quit).
    pub fn shutdown(&self) {
        // Dropping a Control restores the header's motherboard mode.
        self.0.controls.borrow_mut().clear();
    }
}

impl Inner {
    fn update_display(&self, cpu: &Cpu) {
        let Some(device) = cooler::find() else {
            self.button.set_active(false);
            self.button.set_visible(false);
            return;
        };
        self.button.set_visible(true);
        let short_name = device.name.split_whitespace().next().unwrap_or(device.name);
        if self.button_label.text() != short_name {
            self.button_label.set_text(short_name);
            self.display_group
                .set_title(&format!("{} display", device.name));
        }

        if !self.show_row.is_active() {
            self.show_row.set_subtitle("Off");
            return;
        }

        let fahrenheit = self.fahrenheit_row.is_active();
        let celsius = cpu.temperature_celsius.unwrap_or(0.0);
        let temperature = if fahrenheit {
            celsius * 9.0 / 5.0 + 32.0
        } else {
            celsius
        };
        self.display.send(&Reading {
            power_watts: cpu.power_draw_w.unwrap_or(0.0).max(0.0).round() as u16,
            temperature: temperature.round(),
            fahrenheit,
            usage_percent: cpu.total_usage_percent.round().clamp(0.0, 100.0) as u8,
            frequency_mhz: cpu.current_frequency_mhz.min(u16::MAX as u64) as u16,
        });

        self.show_row.set_subtitle(match self.display.link() {
            Link::Idle => "",
            Link::Connected => "Connected",
            Link::NoPermission => SETUP_HINT,
            Link::NotFound => "Not responding",
        });
    }

    fn profile(&self, id: &str) -> Profile {
        self.profiles.borrow().get(id).copied().unwrap_or_default()
    }

    /// Change the selected fan's settings from its rows.
    fn edit_profile(&self, change: impl FnOnce(&mut Profile)) {
        if self.loading.get() {
            return;
        }
        let Some(id) = self.selected.borrow().clone() else {
            return;
        };
        {
            let mut profiles = self.profiles.borrow_mut();
            change(profiles.entry(id).or_default());
        }
        self.save_profiles();
        self.apply_fans();
        self.show_fan_state();
    }

    /// Find the fans, and give each one that spins a button.
    fn refresh_fans(&self) {
        if self.headers.borrow().is_empty() {
            // Also picks up a driver loaded after SysMo started.
            self.headers.replace(fans::headers());
        }

        let shown: Vec<String> = {
            let headers = self.headers.borrow();
            let mut seen = self.seen_spinning.borrow_mut();
            for header in headers.iter() {
                if header.rpm().is_some_and(|rpm| rpm > 0) {
                    seen.insert(header.id.clone());
                }
            }
            headers
                .iter()
                .filter(|header| seen.contains(&header.id) || self.profile(&header.id).fixed)
                .map(|header| header.id.clone())
                .collect()
        };

        let has_headers = !self.headers.borrow().is_empty();
        let has_fans = !shown.is_empty();
        self.fan_row.set_visible(has_fans);
        self.fixed_row.set_visible(has_fans);
        self.speed_row.set_visible(has_fans);
        self.missing_row.set_visible(!has_fans);
        if !has_headers {
            self.missing_row.set_title("No fan control found");
            self.missing_row
                .set_subtitle("The motherboard fan driver is not loaded (see README)");
        } else if !has_fans {
            self.missing_row.set_title("No spinning fans found");
            self.missing_row
                .set_subtitle("Fans appear here once they report a speed");
        }

        if *self.shown.borrow() == shown {
            return;
        }
        self.shown.replace(shown.clone());

        let selected = self.selected.borrow().clone();
        let selected = selected
            .filter(|id| shown.contains(id))
            .or_else(|| default_fan(&self.headers.borrow(), &shown));

        while let Some(child) = self.fan_buttons.first_child() {
            self.fan_buttons.remove(&child);
        }
        let mut group: Option<gtk::ToggleButton> = None;
        for id in &shown {
            let label = self
                .headers
                .borrow()
                .iter()
                .find(|header| &header.id == id)
                .map(|header| header.label.clone())
                .unwrap_or_default();
            let fan_button = gtk::ToggleButton::with_label(&label);
            fan_button.add_css_class("fan-button");
            fan_button.set_group(group.as_ref());
            fan_button.set_active(selected.as_ref() == Some(id));
            let weak = self.weak_self.clone();
            let id = id.clone();
            fan_button.connect_toggled(move |fan_button| {
                if let (true, Some(inner)) = (fan_button.is_active(), weak.upgrade()) {
                    inner.select(&id);
                }
            });
            self.fan_buttons.append(&fan_button);
            group.get_or_insert(fan_button);
        }

        if let Some(id) = selected {
            self.select(&id);
        }
    }

    /// Show `id`'s own settings in the rows.
    fn select(&self, id: &str) {
        self.selected.replace(Some(id.to_owned()));
        if let Some(settings) = &self.settings {
            let _ = settings.set_string(KEY_FAN_SELECTED, id);
        }
        let profile = self.profile(id);
        self.loading.set(true);
        self.fixed_row.set_active(profile.fixed);
        self.speed_row.set_value(profile.percent as f64);
        self.loading.set(false);
        self.show_fan_state();
    }

    /// Bring every fan in line with its settings and the CPU temperature.
    fn apply_fans(&self) {
        let headers = self.headers.borrow();
        let profiles = self.profiles.borrow();
        let mut controls = self.controls.borrow_mut();
        let mut errors = self.errors.borrow_mut();

        // Dropping a Control hands its fan back to the motherboard.
        controls.retain(|id, _| profiles.get(id).is_some_and(|profile| profile.fixed));

        for header in headers.iter() {
            let Some(profile) = profiles.get(&header.id).filter(|profile| profile.fixed) else {
                errors.remove(&header.id);
                continue;
            };
            let percent = if self.hot.get() { 100 } else { profile.percent };
            let result = match controls.get_mut(&header.id) {
                Some(control) => control.set_percent(percent),
                None => self.take_control(header).and_then(|mut control| {
                    let result = control.set_percent(percent);
                    controls.insert(header.id.clone(), control);
                    result
                }),
            };
            match result {
                Ok(()) => {
                    errors.remove(&header.id);
                }
                Err(e) => {
                    errors.insert(header.id.clone(), describe(&e));
                }
            }
        }
    }

    /// Live speed and state of the selected fan.
    fn show_fan_state(&self) {
        let Some(id) = self.selected.borrow().clone() else {
            return;
        };
        let header = self
            .headers
            .borrow()
            .iter()
            .find(|header| header.id == id)
            .cloned();
        let Some(header) = header else {
            return;
        };
        self.fan_row.set_title(&header.label);
        self.fan_row.set_subtitle(&match header.rpm() {
            Some(rpm) => format!("{rpm} RPM"),
            None => "No speed reading".to_owned(),
        });

        let profile = self.profile(&id);
        let subtitle = if let Some(error) = self.errors.borrow().get(&id) {
            error.clone()
        } else if !profile.fixed {
            "Off: the motherboard sets it".to_owned()
        } else if self.hot.get() {
            "Full speed: the CPU is above 85 °C".to_owned()
        } else {
            format!("Held at {}%", profile.percent)
        };
        self.fixed_row.set_subtitle(&subtitle);
    }

    fn take_control(&self, header: &Header) -> io::Result<Control> {
        let Some(settings) = &self.settings else {
            return Control::take(header, None);
        };
        let mut modes: HashMap<String, String> =
            settings.value(KEY_FAN_AUTO_MODES).get().unwrap_or_default();
        let control = Control::take(header, modes.get(&header.id).map(String::as_str))?;
        modes.insert(header.id.clone(), control.auto_mode().to_owned());
        let _ = settings.set_value(KEY_FAN_AUTO_MODES, &modes.to_variant());
        Ok(control)
    }

    fn save_profiles(&self) {
        let Some(settings) = &self.settings else {
            return;
        };
        let stored: HashMap<String, (bool, i32)> = self
            .profiles
            .borrow()
            .iter()
            .map(|(id, profile)| (id.clone(), (profile.fixed, profile.percent as i32)))
            .collect();
        let _ = settings.set_value(KEY_FAN_PROFILES, &stored.to_variant());
    }
}

fn load_profiles(settings: &gio::Settings) -> HashMap<String, Profile> {
    let stored: HashMap<String, (bool, i32)> =
        settings.value(KEY_FAN_PROFILES).get().unwrap_or_default();
    stored
        .into_iter()
        .map(|(id, (fixed, percent))| {
            let percent = percent.clamp(MIN_PERCENT as i32, 100) as u8;
            (id, Profile { fixed, percent })
        })
        .collect()
}

fn describe(error: &io::Error) -> String {
    if error.kind() == io::ErrorKind::PermissionDenied {
        SETUP_HINT.to_owned()
    } else {
        format!("Could not set the fan: {error}")
    }
}

/// The fan selected first: ASUS boards wire CPU_FAN to the second channel,
/// otherwise the first fan shown.
fn default_fan(headers: &[Header], shown: &[String]) -> Option<String> {
    shown
        .iter()
        .find(|id| id.ends_with(":2") && headers.iter().any(|header| &header.id == *id))
        .or_else(|| shown.first())
        .cloned()
}
