/* main.rs
 *
 * SysMo - a compact, GNOME System Monitor style resources view powered by
 * the Mission Center "magpie" engine.
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

mod config;
mod dbus;
mod engine;
mod graph;
mod sections;
mod units;
mod window;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gio, glib};

fn main() -> glib::ExitCode {
    glib::set_prgname(Some("sysmo"));
    glib::set_application_name(config::APP_NAME);

    let app = adw::Application::builder()
        .application_id(config::APP_ID)
        .flags(gio::ApplicationFlags::default())
        .build();

    app.connect_startup(|_| {
        adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);

        let provider = gtk::CssProvider::new();
        provider.load_from_string(include_str!("style.css"));
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
    });

    // Session logout or a terminal Ctrl+C: quit like the user would with
    // Ctrl+Q, so the engine is stopped and window state is saved.
    let terminate = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        let _ = signal_hook::flag::register(signal, terminate.clone());
    }
    glib::timeout_add_local(Duration::from_millis(250), {
        let app = app.clone();
        move || {
            if !terminate.load(Ordering::Relaxed) {
                return glib::ControlFlow::Continue;
            }
            if app.windows().is_empty() {
                app.quit();
            } else {
                // The window's close handler only hides the window (the
                // gauges keep working); "quit" really stops everything.
                app.activate_action("quit", None);
            }
            glib::ControlFlow::Break
        }
    });

    app.connect_activate(|app| {
        // Single instance: a second launch (e.g. from the app grid while the
        // autostarted copy is running) just raises the existing window.
        if let Some(existing) = app.active_window() {
            existing.present();
            return;
        }
        window::SysmoWindow::new(app).present();
    });

    app.run()
}
