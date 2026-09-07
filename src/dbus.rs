/* dbus.rs - publishes the five gauge figures on the session bus so the
 * "SysMo Gauges" GNOME Shell extension can draw them in the top bar.
 *
 * Interface org.sysmo.SysMo.Gauges at /org/sysmo/SysMo/Gauges:
 *   Values  a{sd}  cpu-load, cpu-temp, gpu-load, gpu-vram, gpu-temp
 *   Names   a{ss}  cpu, gpu (hardware names)
 *   Details a{ss}  extra text per gauge key (e.g. "15.2 GB of 17.1 GB")
 * Changes are announced with org.freedesktop.DBus.Properties.PropertiesChanged.
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;
use gtk::glib::{self, g_warning};

pub const OBJECT_PATH: &str = "/org/sysmo/SysMo/Gauges";
pub const INTERFACE: &str = "org.sysmo.SysMo.Gauges";

const INTROSPECTION_XML: &str = r#"
<node>
  <interface name="org.sysmo.SysMo.Gauges">
    <property name="Values" type="a{sd}" access="read"/>
    <property name="Names" type="a{ss}" access="read"/>
    <property name="Details" type="a{ss}" access="read"/>
  </interface>
</node>
"#;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GaugeState {
    pub values: HashMap<String, f64>,
    pub names: HashMap<String, String>,
    pub details: HashMap<String, String>,
}

impl GaugeState {
    fn property(&self, name: &str) -> glib::Variant {
        match name {
            "Values" => self.values.to_variant(),
            "Names" => self.names.to_variant(),
            "Details" => self.details.to_variant(),
            _ => HashMap::<String, String>::new().to_variant(),
        }
    }
}

pub struct GaugeExport {
    connection: gio::DBusConnection,
    state: Rc<RefCell<GaugeState>>,
    registration: Option<gio::RegistrationId>,
}

impl GaugeExport {
    /// Register the object on the application's session-bus connection.
    pub fn new(app: &adw::Application) -> Option<Self> {
        let connection = app.dbus_connection()?;

        let node = match gio::DBusNodeInfo::for_xml(INTROSPECTION_XML) {
            Ok(node) => node,
            Err(e) => {
                g_warning!("SysMo::DBus", "Invalid introspection data: {}", e);
                return None;
            }
        };
        let interface = node.lookup_interface(INTERFACE)?;

        let state = Rc::new(RefCell::new(GaugeState::default()));
        let registration = connection
            .register_object(OBJECT_PATH, &interface)
            .property({
                let state = state.clone();
                move |_connection, _sender, _path, _interface, name| state.borrow().property(name)
            })
            .build();

        match registration {
            Ok(id) => Some(Self {
                connection,
                state,
                registration: Some(id),
            }),
            Err(e) => {
                g_warning!("SysMo::DBus", "Failed to export {}: {}", OBJECT_PATH, e);
                None
            }
        }
    }

    /// Store a new snapshot and notify listeners. Values change every
    /// second; names and details are only announced when they change.
    pub fn publish(&self, new_state: GaugeState) {
        let mut changed: HashMap<String, glib::Variant> = HashMap::new();
        {
            let mut state = self.state.borrow_mut();
            if state.names != new_state.names {
                changed.insert("Names".to_owned(), new_state.names.to_variant());
            }
            if state.details != new_state.details {
                changed.insert("Details".to_owned(), new_state.details.to_variant());
            }
            changed.insert("Values".to_owned(), new_state.values.to_variant());
            *state = new_state;
        }

        let arguments = (INTERFACE, changed, Vec::<String>::new()).to_variant();
        if let Err(e) = self.connection.emit_signal(
            None,
            OBJECT_PATH,
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
            Some(&arguments),
        ) {
            g_warning!("SysMo::DBus", "Failed to emit PropertiesChanged: {}", e);
        }
    }
}

impl Drop for GaugeExport {
    fn drop(&mut self) {
        if let Some(id) = self.registration.take() {
            let _ = self.connection.unregister_object(id);
        }
    }
}
