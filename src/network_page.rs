/* network_page.rs - the Network tab: every SysMo on the local network, one
 * slim row each in two columns: its number (the last part of its address),
 * then a blue CPU badge and a yellow GPU badge, each with load | temperature.
 * Rows are green while the node works and red while it idles; a temperature
 * above 80 °C sits in a yellow box, above 86 °C in a red one.
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::glib;

use crate::icons;
use crate::lan::{self, Activity, Report, Watcher};
use crate::sections::set_tooltip;

/// A temperature above this many °C sits in a yellow box...
const WARM_ABOVE: f32 = 80.0;
/// ...and above this one in a red box.
const HOT_ABOVE: f32 = 86.0;

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
    row: gtk::Box,
    number: gtk::Label,
    cpu: gtk::Label,
    cpu_temp: gtk::Label,
    gpu: gtk::Label,
    gpu_temp: gtk::Label,
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
    summary: gtk::Label,
    searching: gtk::Label,
    grid: gtk::Grid,
    own_id: String,
    watcher: RefCell<Option<Watcher>>,
    tick: RefCell<Option<glib::SourceId>>,
    last_discover: Cell<Option<Instant>>,
    nodes: RefCell<HashMap<String, Node>>,
}

impl NetworkPage {
    pub fn new() -> Self {
        let scan_button = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text("Look for nodes now")
            .valign(gtk::Align::Center)
            .build();
        scan_button.add_css_class("flat");
        let summary = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        summary.add_css_class("node-summary");
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        top.append(&summary);
        top.append(&scan_button);

        let searching = gtk::Label::builder()
            .label(format!(
                "Looking for SysMo on this network…\nNodes appear within a minute. \
                 Each needs SysMo or its node reporter running, and UDP port {} open.",
                lan::PORT
            ))
            .wrap(true)
            .xalign(0.0)
            .build();
        searching.add_css_class("node-summary");
        let grid = gtk::Grid::builder()
            .column_homogeneous(true)
            .row_homogeneous(true)
            .column_spacing(5)
            .row_spacing(6)
            .build();

        let column = gtk::Box::new(gtk::Orientation::Vertical, 6);
        column.set_margin_top(4);
        column.set_margin_bottom(10);
        column.set_margin_start(8);
        column.set_margin_end(8);
        column.append(&top);
        column.append(&searching);
        column.append(&grid);
        let root = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&column)
            .build();

        let page = Self(Rc::new_cyclic(|weak| Inner {
            weak_self: weak.clone(),
            root,
            summary,
            searching,
            grid,
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
                self.summary
                    .set_text(&format!("Can't use the network: {e}"));
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
            self.grid.remove(&node.row);
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
        let removed = !gone.is_empty();
        for id in gone {
            if let Some(node) = self.nodes.borrow_mut().remove(&id) {
                self.grid.remove(&node.row);
            }
        }
        if removed {
            self.reorder();
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
            self.refresh(now);
        } else if let Some(node) = self.nodes.borrow().get(&id) {
            // Only this node's row; the once-a-second tick does the rest.
            self.show(&id, node, now);
        }
    }

    /// A row: state stripe, number, then the CPU badge with load | temperature
    /// and the GPU badge with load | temperature. Every number sits on the
    /// same baseline, whatever its size.
    fn new_node(&self, status: &lan::Status, ip: IpAddr, now: Instant) -> Node {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        row.add_css_class("node-line");
        let stripe = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        stripe.add_css_class("node-stripe");
        row.append(&stripe);
        let number = gtk::Label::builder()
            .width_chars(4)
            .xalign(0.0)
            .valign(gtk::Align::BaselineFill)
            .build();
        number.add_css_class("node-number");
        number.add_css_class("tnum");
        row.append(&number);

        let group = |badge: gtk::DrawingArea| {
            let group = gtk::Box::new(gtk::Orientation::Horizontal, 2);
            group.set_hexpand(true);
            group.set_valign(gtk::Align::BaselineFill);
            group.append(&badge);
            let value = gtk::Label::builder()
                .width_chars(3)
                .xalign(1.0)
                .valign(gtk::Align::BaselineFill)
                .build();
            value.add_css_class("node-value");
            value.add_css_class("tnum");
            group.append(&value);
            let divider = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            divider.add_css_class("node-divider");
            group.append(&divider);
            let temp = gtk::Label::builder()
                .width_chars(3)
                .xalign(0.0)
                .valign(gtk::Align::BaselineCenter)
                .build();
            temp.add_css_class("node-temp");
            temp.add_css_class("tnum");
            group.append(&temp);
            row.append(&group);
            (value, temp)
        };
        let (cpu, cpu_temp) = group(icons::cpu_badge(16));
        let (gpu, gpu_temp) = group(icons::gpu_badge(16));
        Node {
            status: status.clone(),
            ip,
            last_seen: now,
            activity: Activity::default(),
            row,
            number,
            cpu,
            cpu_temp,
            gpu,
            gpu_temp,
        }
    }

    /// This PC first, then by address, filling the left column top to bottom
    /// and then the right one.
    fn reorder(&self) {
        let nodes = self.nodes.borrow();
        let mut ids: Vec<&String> = nodes.keys().collect();
        ids.sort_by_key(|id| (**id != self.own_id, nodes[*id].ip, (*id).clone()));
        let rows = ids.len().div_ceil(2).max(1);
        for (i, id) in ids.iter().enumerate() {
            let row = &nodes[*id].row;
            if row.parent().is_some() {
                self.grid.remove(row);
            }
            self.grid
                .attach(row, (i / rows) as i32, (i % rows) as i32, 1, 1);
        }
    }

    /// Bring one node's row up to date.
    fn show(&self, id: &str, node: &Node, now: Instant) -> State {
        let state = node.state(now);
        let status = &node.status;
        let this_pc = id == self.own_id;
        node.number.set_text(&number(node.ip));
        node.cpu.set_text(&format!("{:.0}", status.cpu));
        node.gpu
            .set_text(&status.gpu.map_or("–".to_owned(), |gpu| format!("{gpu:.0}")));
        show_temperature(&node.cpu_temp, status.cpu_temp);
        show_temperature(&node.gpu_temp, status.gpu_temp);

        // The name and full address, which the row has no room for.
        let mut tooltip = format!("{} · {}", status.name, node.ip);
        if this_pc {
            tooltip.push_str("\nThis PC");
        }
        match status.gpus {
            0 => tooltip.push_str("\nNo GPU reading"),
            1 => {}
            n => tooltip.push_str(&format!("\nGPU: average of its {n} GPUs")),
        }
        set_tooltip(&node.row, Some(&tooltip));

        let class = match state {
            State::Working => "node-working",
            State::Idle => "node-idle",
            State::NoReply => "node-no-reply",
        };
        for other in ["node-working", "node-idle", "node-no-reply"] {
            if other != class {
                node.row.remove_css_class(other);
            }
        }
        node.row.add_css_class(class);
        if this_pc {
            node.row.add_css_class("node-this-pc");
        }
        state
    }

    /// Bring every row and the summary up to date.
    fn refresh(&self, now: Instant) {
        let nodes = self.nodes.borrow();
        let (mut working, mut idle, mut no_reply) = (0, 0, 0);
        for (id, node) in nodes.iter() {
            match self.show(id, node, now) {
                State::Working => working += 1,
                State::Idle => idle += 1,
                State::NoReply => no_reply += 1,
            }
        }

        self.searching.set_visible(nodes.is_empty());
        let mut parts = Vec::new();
        if !nodes.is_empty() {
            parts.push(format!("{} nodes", nodes.len()));
            parts.push(format!("{working} working"));
            parts.push(format!("{idle} idle"));
        }
        if no_reply > 0 {
            parts.push(format!("{no_reply} not replying"));
        }
        self.summary.set_text(&parts.join(" · "));
    }
}

/// "68°", in a yellow box above 80 °C and a red one above 86 °C; "–" when
/// the node sends none (no sensor, or an older SysMo).
fn show_temperature(label: &gtk::Label, celsius: Option<f32>) {
    let rounded = celsius.map(f32::round);
    label.set_text(&rounded.map_or("–".to_owned(), |t| format!("{t:.0}°")));
    let heat = match rounded {
        Some(t) if t > HOT_ABOVE => Some("node-temp-hot"),
        Some(t) if t > WARM_ABOVE => Some("node-temp-warm"),
        _ => None,
    };
    for class in ["node-temp-warm", "node-temp-hot"] {
        if Some(class) == heat {
            label.add_css_class(class);
        } else {
            label.remove_css_class(class);
        }
    }
}

/// A node's number: the last part of its IPv4 address, such as ".223".
fn number(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(ip) => format!(".{}", ip.octets()[3]),
        IpAddr::V6(ip) => ip.to_string(),
    }
}
