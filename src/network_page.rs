/* network_page.rs - the Network tab: every SysMo on the local network with
 * its CPU and GPU load, green while it works and red while it idles.
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::{gio, glib};

use crate::lan::{self, Activity, Report, Reporter, Watcher};

const KEY_SHARE: &str = "network-share";

/// One "who's there?" a minute finds new nodes.
const DISCOVER_EVERY: Duration = Duration::from_secs(60);
/// A node silent for this long shows "No reply"...
const STALE_AFTER: Duration = Duration::from_secs(5);
/// ...and is dropped after this long.
const GONE_AFTER: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Working,
    Idle,
    NoReply,
}

struct Node {
    status: lan::Status,
    ip: IpAddr,
    last_seen: Instant,
    activity: Activity,
    row: adw::ActionRow,
    cpu: gtk::Label,
    gpu: gtk::Label,
    badge: gtk::Label,
}

impl Node {
    fn state(&self, now: Instant) -> State {
        if now.duration_since(self.last_seen) > STALE_AFTER {
            State::NoReply
        } else if self.activity.busy() {
            State::Working
        } else {
            State::Idle
        }
    }
}

#[derive(Clone)]
pub struct NetworkPage(Rc<Inner>);

struct Inner {
    weak_self: Weak<Inner>,
    root: gtk::ScrolledWindow,
    nodes_group: adw::PreferencesGroup,
    searching_row: adw::ActionRow,
    own_id: String,
    watcher: RefCell<Option<Watcher>>,
    tick: RefCell<Option<glib::SourceId>>,
    last_discover: Cell<Option<Instant>>,
    nodes: RefCell<HashMap<String, Node>>,
}

impl NetworkPage {
    pub fn new(settings: Option<gio::Settings>, reporter: Option<Rc<Reporter>>) -> Self {
        let scan_button = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text("Look for nodes now")
            .valign(gtk::Align::Center)
            .build();
        scan_button.add_css_class("flat");
        let searching_row = adw::ActionRow::builder()
            .title("Looking for SysMo on this network…")
            .subtitle(format!(
                "Nodes appear within a minute. Each needs SysMo running with \
                 sharing on, and UDP port {} open.",
                lan::PORT
            ))
            .build();
        let nodes_group = adw::PreferencesGroup::builder().title("Nodes").build();
        nodes_group.set_header_suffix(Some(&scan_button));
        nodes_group.add(&searching_row);

        let share_row = adw::SwitchRow::builder()
            .title("Share this PC's load")
            .subtitle("Other SysMo on this network can see its CPU and GPU load")
            .active(true)
            .build();
        let share_group = adw::PreferencesGroup::builder().title("This PC").build();
        share_group.add(&share_row);
        match &reporter {
            Some(reporter) => {
                if let Some(settings) = &settings {
                    settings.bind(KEY_SHARE, &share_row, "active").build();
                }
                reporter.set_enabled(share_row.is_active());
                let reporter = reporter.clone();
                share_row.connect_active_notify(move |row| reporter.set_enabled(row.is_active()));
            }
            None => {
                share_row.set_active(false);
                share_row.set_sensitive(false);
                share_row.set_subtitle(&format!(
                    "UDP port {} is in use by another program, so this PC can't be shared",
                    lan::PORT
                ));
            }
        }

        let column = gtk::Box::new(gtk::Orientation::Vertical, 18);
        column.set_margin_top(8);
        column.set_margin_bottom(12);
        column.set_margin_start(12);
        column.set_margin_end(12);
        column.append(&nodes_group);
        column.append(&share_group);
        let root = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&column)
            .build();

        let page = Self(Rc::new_cyclic(|weak| Inner {
            weak_self: weak.clone(),
            root,
            nodes_group,
            searching_row,
            own_id: lan::node_id(),
            watcher: RefCell::new(None),
            tick: RefCell::new(None),
            last_discover: Cell::new(None),
            nodes: RefCell::new(HashMap::new()),
        }));

        let weak = Rc::downgrade(&page.0);
        scan_button.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                inner.discover();
            }
        });
        page.0.refresh(Instant::now());
        page
    }

    pub fn widget(&self) -> &gtk::ScrolledWindow {
        &self.0.root
    }

    /// Watch the network only while the tab is on screen.
    pub fn set_active(&self, active: bool) {
        if active {
            self.0.start();
        } else {
            self.0.stop();
        }
    }
}

impl Inner {
    fn start(&self) {
        if self.watcher.borrow().is_some() {
            return;
        }
        let (sender, reports) = async_channel::unbounded::<Report>();
        let target = SocketAddr::from((Ipv4Addr::BROADCAST, lan::PORT));
        match Watcher::start(target, sender) {
            Ok(watcher) => {
                self.watcher.replace(Some(watcher));
            }
            Err(e) => {
                self.nodes_group
                    .set_description(Some(&format!("Can't use the network: {e}")));
                return;
            }
        }

        glib::spawn_future_local({
            let weak = self.weak_self.clone();
            async move {
                while let Ok(report) = reports.recv().await {
                    match weak.upgrade() {
                        Some(inner) => inner.on_report(report),
                        None => break,
                    }
                }
            }
        });
        let weak = self.weak_self.clone();
        let tick = glib::timeout_add_local(Duration::from_secs(1), move || match weak.upgrade() {
            Some(inner) => {
                inner.on_tick();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
        self.tick.replace(Some(tick));
        self.discover();
    }

    fn stop(&self) {
        if let Some(tick) = self.tick.take() {
            tick.remove();
        }
        // Dropping the watcher ends its thread, which ends the report loop.
        if let Some(watcher) = self.watcher.take() {
            watcher.bye(self.nodes.borrow().values().map(|node| node.ip));
        }
        for (_, node) in self.nodes.borrow_mut().drain() {
            self.nodes_group.remove(&node.row);
        }
        self.last_discover.set(None);
        self.refresh(Instant::now());
    }

    fn discover(&self) {
        if let Some(watcher) = self.watcher.borrow().as_ref() {
            watcher.discover();
            self.last_discover.set(Some(Instant::now()));
        }
    }

    fn on_tick(&self) {
        let now = Instant::now();
        let due = self
            .last_discover
            .get()
            .is_none_or(|last| now.duration_since(last) >= DISCOVER_EVERY);
        if due {
            self.discover();
        }

        let gone: Vec<String> = self
            .nodes
            .borrow()
            .iter()
            .filter(|(_, node)| now.duration_since(node.last_seen) > GONE_AFTER)
            .map(|(id, _)| id.clone())
            .collect();
        for id in gone {
            if let Some(node) = self.nodes.borrow_mut().remove(&id) {
                self.nodes_group.remove(&node.row);
            }
        }
        self.refresh(now);
    }

    pub(crate) fn on_report(&self, report: Report) {
        let now = Instant::now();
        let id = report.status.id.clone();
        let is_new = !self.nodes.borrow().contains_key(&id);
        if is_new {
            let node = self.new_node(&report.status, report.from, now);
            self.nodes.borrow_mut().insert(id.clone(), node);
        }
        if let Some(node) = self.nodes.borrow_mut().get_mut(&id) {
            node.activity
                .push(now, report.status.cpu, report.status.gpu);
            node.status = report.status;
            node.ip = report.from;
            node.last_seen = now;
        }
        if is_new {
            self.reorder();
        }
        self.refresh(now);
    }

    fn new_node(&self, status: &lan::Status, ip: IpAddr, now: Instant) -> Node {
        let value_label = || {
            let label = gtk::Label::new(None);
            label.add_css_class("node-value");
            label.add_css_class("tnum");
            label.set_width_chars(8);
            label.set_xalign(1.0);
            label
        };
        let cpu = value_label();
        let gpu = value_label();
        let badge = gtk::Label::new(None);
        badge.add_css_class("node-badge");
        badge.set_width_chars(8);
        badge.set_valign(gtk::Align::Center);

        let row = adw::ActionRow::new();
        row.add_css_class("node-row");
        row.add_suffix(&cpu);
        row.add_suffix(&gpu);
        row.add_suffix(&badge);
        Node {
            status: status.clone(),
            ip,
            last_seen: now,
            activity: Activity::default(),
            row,
            cpu,
            gpu,
            badge,
        }
    }

    /// This PC first, then by name, so rows do not jump around.
    fn reorder(&self) {
        let nodes = self.nodes.borrow();
        let mut ids: Vec<&String> = nodes.keys().collect();
        ids.sort_by_key(|id| {
            (
                **id != self.own_id,
                nodes[*id].status.name.to_lowercase(),
                (*id).clone(),
            )
        });
        for id in ids {
            let row = &nodes[id].row;
            if row.parent().is_some() {
                self.nodes_group.remove(row);
            }
            self.nodes_group.add(row);
        }
    }

    /// Bring every row and the summary up to date.
    fn refresh(&self, now: Instant) {
        let nodes = self.nodes.borrow();
        let (mut working, mut idle, mut no_reply) = (0, 0, 0);
        for (id, node) in nodes.iter() {
            let state = node.state(now);
            match state {
                State::Working => working += 1,
                State::Idle => idle += 1,
                State::NoReply => no_reply += 1,
            }

            let status = &node.status;
            node.row.set_title(&status.name);
            node.row.set_subtitle(&if *id == self.own_id {
                format!("This PC · {}", node.ip)
            } else {
                node.ip.to_string()
            });
            node.cpu.set_text(&format!("CPU {:.0}%", status.cpu));
            node.gpu.set_text(&match status.gpu {
                Some(gpu) => format!("GPU {gpu:.0}%"),
                None => "GPU –".to_owned(),
            });
            node.gpu.set_tooltip_text(match status.gpus {
                0 => Some("No GPU reading"),
                1 => None,
                _ => Some("Average of its GPUs"),
            });
            let (class, text) = match state {
                State::Working => ("node-working", "Working"),
                State::Idle => ("node-idle", "Idle"),
                State::NoReply => ("node-no-reply", "No reply"),
            };
            for other in ["node-working", "node-idle", "node-no-reply"] {
                if other != class {
                    node.row.remove_css_class(other);
                }
            }
            node.row.add_css_class(class);
            node.badge.set_text(text);
        }

        self.searching_row.set_visible(nodes.is_empty());
        let mut summary = if nodes.is_empty() {
            String::new()
        } else {
            let mut parts = vec![format!("{} working", working), format!("{} idle", idle)];
            if no_reply > 0 {
                parts.push(format!("{no_reply} not replying"));
            }
            parts.join(" · ")
        };
        if let Some(last) = self.last_discover.get() {
            let next = DISCOVER_EVERY.saturating_sub(now.duration_since(last));
            if !summary.is_empty() {
                summary.push_str(" · ");
            }
            summary.push_str(&format!("next scan in {} s", next.as_secs()));
        }
        self.nodes_group
            .set_description(Some(summary.as_str()).filter(|summary| !summary.is_empty()));
    }
}
