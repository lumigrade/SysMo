/* cooler_panel.rs - the drop-down behind the cooler button in the CPU
 * section's title: what the DeepCool cooler's display shows, and the CPU
 * fan's speed.
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

use std::cell::{Cell, RefCell};
use std::io;
use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;

use crate::cooler::{self, Link, Reading};
use crate::engine::Cpu;
use crate::fans::{self, Control, Header};

const KEY_DISPLAY: &str = "cooler-display";
const KEY_FAHRENHEIT: &str = "cooler-fahrenheit";
const KEY_FAN_HEADER: &str = "fan-header";
const KEY_FAN_FIXED: &str = "fan-fixed";
const KEY_FAN_PERCENT: &str = "fan-percent";
const KEY_FAN_AUTO_MODE: &str = "fan-auto-mode";

/// A fixed fan speed is overridden with full speed from this temperature
/// until the CPU is back under `COOL_CELSIUS`.
const HOT_CELSIUS: f32 = 85.0;
const COOL_CELSIUS: f32 = 80.0;

const SETUP_HINT: &str = "Needs the one-time setup (see README)";

#[derive(Clone)]
pub struct CoolerPanel(Rc<Inner>);

struct Inner {
    settings: Option<gio::Settings>,
    display: cooler::Display,
    button: gtk::ToggleButton,
    button_label: gtk::Label,
    revealer: gtk::Revealer,
    display_group: adw::PreferencesGroup,
    show_row: adw::SwitchRow,
    fahrenheit_row: adw::SwitchRow,
    header_row: adw::ComboRow,
    fixed_row: adw::SwitchRow,
    speed_row: adw::SpinRow,
    missing_row: adw::ActionRow,
    headers: RefCell<Vec<Header>>,
    control: RefCell<Option<Control>>,
    fan_error: RefCell<Option<String>>,
    hot: Cell<bool>,
    /// Set while the header list is rebuilt, so that is not taken as a choice.
    filling_headers: Cell<bool>,
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
            .tooltip_text("Cooler display and fan")
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

        let header_row = adw::ComboRow::builder()
            .title("Fan header")
            .model(&gtk::StringList::new(&[]))
            .build();
        let fixed_row = adw::SwitchRow::builder().title("Fixed speed").build();
        let speed_row = adw::SpinRow::with_range(30.0, 100.0, 5.0);
        speed_row.set_title("Speed (%)");
        speed_row.set_subtitle("Full speed above 85 °C");
        speed_row.set_value(60.0);
        let missing_row = adw::ActionRow::builder()
            .title("No fan control found")
            .subtitle("The motherboard fan driver is not loaded (see README)")
            .build();
        let fan_group = adw::PreferencesGroup::builder().title("CPU fan").build();
        fan_group.add(&header_row);
        fan_group.add(&fixed_row);
        fan_group.add(&speed_row);
        fan_group.add(&missing_row);
        fixed_row
            .bind_property("active", &speed_row, "sensitive")
            .sync_create()
            .build();

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

        if let Some(settings) = &settings {
            settings.bind(KEY_DISPLAY, &show_row, "active").build();
            settings
                .bind(KEY_FAHRENHEIT, &fahrenheit_row, "active")
                .build();
            settings.bind(KEY_FAN_FIXED, &fixed_row, "active").build();
            settings.bind(KEY_FAN_PERCENT, &speed_row, "value").build();
        }

        let panel = Self(Rc::new(Inner {
            settings,
            display: cooler::Display::start(),
            button,
            button_label,
            revealer,
            display_group,
            show_row,
            fahrenheit_row,
            header_row,
            fixed_row,
            speed_row,
            missing_row,
            headers: RefCell::new(Vec::new()),
            control: RefCell::new(None),
            fan_error: RefCell::new(None),
            hot: Cell::new(false),
            filling_headers: Cell::new(false),
        }));

        let weak = Rc::downgrade(&panel.0);
        panel.0.fixed_row.connect_active_notify({
            let weak = weak.clone();
            move |_| {
                if let Some(inner) = weak.upgrade() {
                    inner.apply_fan();
                }
            }
        });
        panel.0.speed_row.connect_value_notify({
            let weak = weak.clone();
            move |_| {
                if let Some(inner) = weak.upgrade() {
                    inner.apply_fan();
                }
            }
        });
        panel.0.header_row.connect_selected_notify(move |row| {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            if inner.filling_headers.get() {
                return;
            }
            let id = inner
                .headers
                .borrow()
                .get(row.selected() as usize)
                .map(|header| header.id.clone());
            if let (Some(settings), Some(id)) = (&inner.settings, id) {
                let _ = settings.set_string(KEY_FAN_HEADER, &id);
            }
            inner.apply_fan();
        });

        panel.0.refresh_headers();
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
        self.0.update_fan(cpu);
    }

    /// Hand the fan back to the motherboard (on quit).
    pub fn shutdown(&self) {
        if let Some(mut control) = self.0.control.borrow_mut().take() {
            let _ = control.release();
        }
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

    fn update_fan(&self, cpu: &Cpu) {
        if self.headers.borrow().is_empty() {
            // The driver may have been loaded since SysMo started.
            self.refresh_headers();
        }

        let rpm = self.selected_header().and_then(|header| header.rpm());
        self.header_row.set_subtitle(&match rpm {
            Some(rpm) => format!("{rpm} RPM"),
            None => "No speed reading".to_owned(),
        });

        if let Some(celsius) = cpu.temperature_celsius {
            if celsius >= HOT_CELSIUS {
                self.hot.set(true);
            } else if celsius < COOL_CELSIUS {
                self.hot.set(false);
            }
        }
        self.apply_fan();
    }

    fn refresh_headers(&self) {
        let headers = fans::headers();
        let found = !headers.is_empty();
        self.header_row.set_visible(found);
        self.fixed_row.set_visible(found);
        self.speed_row.set_visible(found);
        self.missing_row.set_visible(!found);
        if !found {
            return;
        }

        let saved = self
            .settings
            .as_ref()
            .map(|settings| settings.string(KEY_FAN_HEADER).to_string())
            .unwrap_or_default();
        let selected = headers
            .iter()
            .position(|header| header.id == saved)
            .unwrap_or_else(|| default_header(&headers));

        let labels: Vec<&str> = headers.iter().map(|header| header.label.as_str()).collect();
        self.filling_headers.set(true);
        self.header_row
            .set_model(Some(&gtk::StringList::new(&labels)));
        self.header_row.set_selected(selected as u32);
        self.filling_headers.set(false);
        if let Some(settings) = &self.settings {
            let _ = settings.set_string(KEY_FAN_HEADER, &headers[selected].id);
        }
        self.headers.replace(headers);
    }

    fn selected_header(&self) -> Option<Header> {
        self.headers
            .borrow()
            .get(self.header_row.selected() as usize)
            .cloned()
    }

    /// Bring the fan in line with the switch, the speed and the temperature.
    fn apply_fan(&self) {
        let header = self.selected_header();
        let fixed = self.fixed_row.is_active() && header.is_some();

        let mut control = self.control.borrow_mut();
        let wrong_header = control.as_ref().is_some_and(|control| {
            Some(control.header_id()) != header.as_ref().map(|header| header.id.as_str())
        });
        if !fixed || wrong_header {
            if let Some(mut old) = control.take() {
                let _ = old.release();
            }
        }

        let result = match (fixed, header) {
            (true, Some(header)) => {
                let taken = match control.take() {
                    Some(existing) => Ok(existing),
                    None => self.take_control(&header),
                };
                taken.and_then(|mut taken| {
                    let percent = if self.hot.get() {
                        100
                    } else {
                        self.speed_row.value().round() as u8
                    };
                    let result = taken.set_percent(percent);
                    *control = Some(taken);
                    result
                })
            }
            _ => Ok(()),
        };
        drop(control);

        self.fan_error.replace(result.err().map(|e| {
            if e.kind() == io::ErrorKind::PermissionDenied {
                SETUP_HINT.to_owned()
            } else {
                format!("Could not set the fan: {e}")
            }
        }));

        let subtitle = if let Some(error) = self.fan_error.borrow().as_ref() {
            error.clone()
        } else if !self.fixed_row.is_active() {
            "Off: the motherboard sets it".to_owned()
        } else if self.hot.get() {
            "Full speed: the CPU is above 85 °C".to_owned()
        } else {
            format!("Held at {:.0}%", self.speed_row.value())
        };
        self.fixed_row.set_subtitle(&subtitle);
    }

    fn take_control(&self, header: &Header) -> io::Result<Control> {
        let saved = self
            .settings
            .as_ref()
            .map(|settings| settings.string(KEY_FAN_AUTO_MODE).to_string())
            .filter(|mode| !mode.is_empty());
        let control = Control::take(header, saved.as_deref())?;
        if let Some(settings) = &self.settings {
            let _ = settings.set_string(KEY_FAN_AUTO_MODE, control.auto_mode());
        }
        Ok(control)
    }
}

/// ASUS boards wire CPU_FAN to the second channel; otherwise take the first
/// header that reports a speed.
fn default_header(headers: &[Header]) -> usize {
    let spinning = |header: &Header| header.rpm().is_some_and(|rpm| rpm > 0);
    headers
        .iter()
        .position(|header| header.id.ends_with(":2") && spinning(header))
        .or_else(|| headers.iter().position(spinning))
        .unwrap_or(0)
}
