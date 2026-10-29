/* network_page.rs - the Network tab: every node on the local network with
 * its CPU and GPU load, green while it works and red while it idles. Nodes
 * running SysMo report themselves; the others are read over SSH.
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
use crate::ssh_nodes::{self, SshMonitor};

const KEY_SHARE: &str = "network-share";
const KEY_SSH_HOSTS: &str = "network-ssh-hosts";
const KEY_SSH_USER: &str = "network-ssh-user";

/// One "who's there?" a minute finds new SysMo nodes.
const DISCOVER_EVERY: Duration = Duration::from_secs(60);
/// A node silent for this long shows "No reply"...
const STALE_AFTER: Duration = Duration::from_secs(5);
/// ...and is dropped after this long.
const GONE_AFTER: Duration = Duration::from_secs(60);

const SSH_PREFIX: &str = "ssh-";

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Working,
    Idle,
    NoReply,
    NoAccess,
}

struct Node {
    status: lan::Status,
    ip: IpAddr,
    last_seen: Instant,
    activity: Activity,
    /// Set for an SSH node that is up but refuses the login.
    no_access: Option<String>,
    row: adw::ActionRow,
    cpu: gtk::Label,
    gpu: gtk::Label,
    badge: gtk::Label,
}

impl Node {
    fn via_ssh(&self) -> bool {
        self.status.id.starts_with(SSH_PREFIX)
    }

    fn state(&self, now: Instant) -> State {
        if self.no_access.is_some() {
            State::NoAccess
        } else if now.duration_since(self.last_seen) > STALE_AFTER {
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
    settings: Option<gio::Settings>,
    root: gtk::ScrolledWindow,
    nodes_group: adw::PreferencesGroup,
    searching_row: adw::ActionRow,
    ssh_group: adw::PreferencesGroup,
    hosts_row: adw::EntryRow,
    own_id: String,
    watcher: RefCell<Option<Watcher>>,
    ssh: RefCell<Option<SshMonitor>>,
    /// Applied (not just typed) SSH addresses and user.
    ssh_hosts: RefCell<String>,
    ssh_user: RefCell<String>,
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
            .title("Looking for nodes…")
            .subtitle(format!(
                "Computers running SysMo appear within a minute (UDP port {} open). \
                 Add the others under Nodes without SysMo.",
                lan::PORT
            ))
            .build();
        let nodes_group = adw::PreferencesGroup::builder().title("Nodes").build();
        nodes_group.set_header_suffix(Some(&scan_button));
        nodes_group.add(&searching_row);

        let (ssh_hosts, ssh_user) = match &settings {
            Some(settings) => (
                settings.string(KEY_SSH_HOSTS).to_string(),
                settings.string(KEY_SSH_USER).to_string(),
            ),
            None => Default::default(),
        };
        let hosts_row = adw::EntryRow::builder()
            .title("Addresses, e.g. 192.168.1.100-130")
            .text(&ssh_hosts)
            .show_apply_button(true)
            .build();
        let user_row = adw::EntryRow::builder()
            .title("SSH user (blank: as in ~/.ssh/config)")
            .text(&ssh_user)
            .show_apply_button(true)
            .build();
        let ssh_group = adw::PreferencesGroup::builder()
            .title("Nodes without SysMo")
            .build();
        ssh_group.add(&hosts_row);
        ssh_group.add(&user_row);

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
        column.append(&ssh_group);
        column.append(&share_group);
        let root = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&column)
            .build();

        let page = Self(Rc::new_cyclic(|weak| Inner {
            weak_self: weak.clone(),
            settings,
            root,
            nodes_group,
            searching_row,
            ssh_group,
            hosts_row,
            own_id: lan::node_id(),
            watcher: RefCell::new(None),
            ssh: RefCell::new(None),
            ssh_hosts: RefCell::new(ssh_hosts),
            ssh_user: RefCell::new(ssh_user),
            tick: RefCell::new(None),
            last_discover: Cell::new(None),
            nodes: RefCell::new(HashMap::new()),
        }));

        let weak = Rc::downgrade(&page.0);
        scan_button.connect_clicked({
            let weak = weak.clone();
            move |_| {
                if let Some(inner) = weak.upgrade() {
                    inner.discover();
                }
            }
        });
        page.0.hosts_row.connect_apply({
            let weak = weak.clone();
            move |row| {
                if let Some(inner) = weak.upgrade() {
                    inner.apply_ssh(Some(row.text().to_string()), None);
                }
            }
        });
        user_row.connect_apply(move |row| {
            if let Some(inner) = weak.upgrade() {
                inner.apply_ssh(None, Some(row.text().trim().to_owned()));
            }
        });
        page.0.describe_ssh();
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
        if self.tick.borrow().is_some() {
            return;
        }
        let (sender, reports) = async_channel::unbounded::<Report>();
        let target = SocketAddr::from((Ipv4Addr::BROADCAST, lan::PORT));
        match Watcher::start(target, sender) {
            Ok(watcher) => {
                self.watcher.replace(Some(watcher));
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
            }
            Err(e) => {
                self.nodes_group
                    .set_description(Some(&format!("Can't use the network: {e}")));
            }
        }
        self.start_ssh();

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
        // Dropping the watcher and the SSH monitor ends their threads,
        // which ends the loops reading from them.
        if let Some(watcher) = self.watcher.take() {
            let sysmo_nodes: Vec<IpAddr> = self
                .nodes
                .borrow()
                .values()
                .filter(|node| !node.via_ssh())
                .map(|node| node.ip)
                .collect();
            watcher.bye(sysmo_nodes);
        }
        self.ssh.replace(None);
        for (_, node) in self.nodes.borrow_mut().drain() {
            self.nodes_group.remove(&node.row);
        }
        self.last_discover.set(None);
        self.refresh(Instant::now());
    }

    /// (Re)connect to the SSH addresses, if the tab is being watched.
    fn start_ssh(&self) {
        self.ssh.replace(None);
        let targets = ssh_nodes::parse_targets(&self.ssh_hosts.borrow()).unwrap_or_default();
        if targets.is_empty() {
            return;
        }
        let user = Some(self.ssh_user.borrow().clone()).filter(|user| !user.is_empty());
        let (sender, events) = async_channel::unbounded::<ssh_nodes::Event>();
        self.ssh
            .replace(Some(SshMonitor::start(targets, user, sender)));
        glib::spawn_future_local({
            let weak = self.weak_self.clone();
            async move {
                while let Ok(event) = events.recv().await {
                    match weak.upgrade() {
                        Some(inner) => inner.on_ssh_event(event),
                        None => break,
                    }
                }
            }
        });
    }

    /// The SSH address or user field was applied.
    fn apply_ssh(&self, hosts: Option<String>, user: Option<String>) {
        if let Some(hosts) = hosts {
            if let Err(error) = ssh_nodes::parse_targets(&hosts) {
                self.hosts_row.add_css_class("error");
                self.ssh_group.set_description(Some(&error));
                return;
            }
            self.hosts_row.remove_css_class("error");
            if let Some(settings) = &self.settings {
                let _ = settings.set_string(KEY_SSH_HOSTS, &hosts);
            }
            self.ssh_hosts.replace(hosts);
        }
        if let Some(user) = user {
            if let Some(settings) = &self.settings {
                let _ = settings.set_string(KEY_SSH_USER, &user);
            }
            self.ssh_user.replace(user);
        }
        self.describe_ssh();

        // Start over with the new list.
        let ssh_ids: Vec<String> = self
            .nodes
            .borrow()
            .values()
            .filter(|node| node.via_ssh())
            .map(|node| node.status.id.clone())
            .collect();
        for id in ssh_ids {
            self.remove_node(&id);
        }
        if self.tick.borrow().is_some() {
            self.start_ssh();
        }
        self.refresh(Instant::now());
    }

    fn describe_ssh(&self) {
        let count = ssh_nodes::parse_targets(&self.ssh_hosts.borrow())
            .map(|targets| targets.len())
            .unwrap_or(0);
        let text = if count == 0 {
            "For computers without SysMo: SysMo logs in with your SSH keys and reads \
             their CPU and GPU load once a second. Nothing is installed on them."
                .to_owned()
        } else {
            format!(
                "Watching {count} address{} over SSH while this tab is open. \
                 Nothing is installed on them.",
                if count == 1 { "" } else { "es" }
            )
        };
        self.ssh_group.set_description(Some(&text));
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

        // Rows for nodes that refuse SSH stay until the next attempt.
        let gone: Vec<String> = self
            .nodes
            .borrow()
            .iter()
            .filter(|(_, node)| {
                node.no_access.is_none() && now.duration_since(node.last_seen) > GONE_AFTER
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in gone {
            self.remove_node(&id);
        }
        self.refresh(now);
    }

    fn remove_node(&self, id: &str) {
        if let Some(node) = self.nodes.borrow_mut().remove(id) {
            self.nodes_group.remove(&node.row);
        }
    }

    /// A node that reports through SysMo; SSH figures for the same address
    /// are left out.
    fn covered_by_sysmo(&self, ip: IpAddr) -> bool {
        let now = Instant::now();
        self.nodes.borrow().values().any(|node| {
            !node.via_ssh() && node.ip == ip && now.duration_since(node.last_seen) <= GONE_AFTER
        })
    }

    fn on_ssh_event(&self, event: ssh_nodes::Event) {
        match event {
            ssh_nodes::Event::Status(report) => {
                if self.covered_by_sysmo(report.from) {
                    self.remove_node(&report.status.id);
                } else {
                    self.on_report(report);
                }
            }
            ssh_nodes::Event::NoAccess { ip, reason } => {
                let id = format!("{SSH_PREFIX}{ip}");
                if self.covered_by_sysmo(ip) {
                    self.remove_node(&id);
                    return;
                }
                let now = Instant::now();
                let is_new = !self.nodes.borrow().contains_key(&id);
                if is_new {
                    let status = lan::Status {
                        id: id.clone(),
                        name: ip.to_string(),
                        cpu: 0.0,
                        gpu: None,
                        gpus: 0,
                    };
                    let node = self.new_node(&status, ip, now);
                    self.nodes.borrow_mut().insert(id.clone(), node);
                }
                if let Some(node) = self.nodes.borrow_mut().get_mut(&id) {
                    node.no_access = Some(reason);
                    node.last_seen = now;
                }
                if is_new {
                    self.reorder();
                }
                self.refresh(now);
            }
        }
    }

    pub(crate) fn on_report(&self, report: Report) {
        let now = Instant::now();
        let id = report.status.id.clone();
        if !id.starts_with(SSH_PREFIX) {
            // The same computer may also be in the SSH list.
            self.remove_node(&format!("{SSH_PREFIX}{}", report.from));
        }
        let is_new = !self.nodes.borrow().contains_key(&id);
        if is_new {
            let node = self.new_node(&report.status, report.from, now);
            self.nodes.borrow_mut().insert(id.clone(), node);
        }
        let renamed = if let Some(node) = self.nodes.borrow_mut().get_mut(&id) {
            node.activity
                .push(now, report.status.cpu, report.status.gpu);
            let renamed = node.status.name != report.status.name;
            node.status = report.status;
            node.ip = report.from;
            node.last_seen = now;
            node.no_access = None;
            renamed
        } else {
            false
        };
        if is_new || renamed {
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
            no_access: None,
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
        let (mut working, mut idle, mut no_reply, mut no_access) = (0, 0, 0, 0);
        for (id, node) in nodes.iter() {
            let state = node.state(now);
            match state {
                State::Working => working += 1,
                State::Idle => idle += 1,
                State::NoReply => no_reply += 1,
                State::NoAccess => no_access += 1,
            }

            let status = &node.status;
            node.row.set_title(&status.name);
            node.row
                .set_subtitle(&match (&node.no_access, node.via_ssh()) {
                    (Some(reason), _) => format!("{} · {reason}", node.ip),
                    (None, true) => format!("{} · SSH", node.ip),
                    (None, false) if *id == self.own_id => format!("This PC · {}", node.ip),
                    (None, false) => node.ip.to_string(),
                });
            if state == State::NoAccess {
                node.cpu.set_text("CPU –");
                node.gpu.set_text("GPU –");
            } else {
                node.cpu.set_text(&format!("CPU {:.0}%", status.cpu));
                node.gpu.set_text(&match status.gpu {
                    Some(gpu) => format!("GPU {gpu:.0}%"),
                    None => "GPU –".to_owned(),
                });
            }
            node.gpu.set_tooltip_text(match status.gpus {
                0 => Some("No GPU reading"),
                1 => None,
                _ => Some("Average of its GPUs"),
            });
            let (class, text) = match state {
                State::Working => ("node-working", "Working"),
                State::Idle => ("node-idle", "Idle"),
                State::NoReply => ("node-no-reply", "No reply"),
                State::NoAccess => ("node-no-reply", "No access"),
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
        let mut parts = Vec::new();
        if !nodes.is_empty() {
            parts.push(format!("{working} working"));
            parts.push(format!("{idle} idle"));
            if no_reply > 0 {
                parts.push(format!("{no_reply} not replying"));
            }
            if no_access > 0 {
                parts.push(format!("{no_access} without access"));
            }
        }
        if let Some(last) = self.last_discover.get() {
            let next = DISCOVER_EVERY.saturating_sub(now.duration_since(last));
            parts.push(format!("next scan in {} s", next.as_secs()));
        }
        let summary = parts.join(" · ");
        self.nodes_group
            .set_description(Some(summary.as_str()).filter(|summary| !summary.is_empty()));
    }
}
