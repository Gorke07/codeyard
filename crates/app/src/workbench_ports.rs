//! Workbench's port forwarding: while connected over SSH, scans the ports listening on the server
//! every few seconds; ports opened after connecting are forwarded
//! automatically (ones open from the start are only listed). The PORTS bottom panel.

use std::time::Duration;

use gpui::{AnyElement, Context, MouseButton, SharedString, div, prelude::*, px};

use super::{Conn, LOCAL_AGENT_ALIAS, Notice, Workbench, center};
use crate::ports;
use crate::ui::{self, Palette, c, icon, layout, text};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Forward {
    pub remote: u16,
    pub local: u16,
}

/// Port forwarding state in the Workbench: established forwards, ports listening on the server,
/// ports from the first scan (not forwarded automatically), the scan task and the manual entry field.
pub struct PortsState {
    pub forwards: Vec<Forward>,
    pub seen: Vec<u16>,
    pub baseline: Option<Vec<u16>>,
    /// Local ports we opened for forwarding (ignored in scans).
    pub local: Vec<u16>,
    pub task: Option<gpui::Task<()>>,
    pub field: gpui::Entity<crate::input::TextField>,
}

impl PortsState {
    pub fn new(field: gpui::Entity<crate::input::TextField>) -> Self {
        Self { forwards: Vec::new(), seen: Vec::new(), baseline: None, local: Vec::new(), task: None, field }
    }
}

/// Scan interval (while connected).
const SCAN_EVERY: Duration = Duration::from_secs(4);

impl Workbench {
    /// The connection port forwarding can use (ssh alias).
    fn ssh_alias(&self) -> Option<String> {
        match &self.remote.conn {
            Conn::Connected { alias } if alias != LOCAL_AGENT_ALIAS => Some(alias.clone()),
            _ => None,
        }
    }

    /// On connect (or reconnect) start the scan; set up the earlier forwards.
    pub(super) fn start_ports(&mut self, cx: &mut Context<Self>) {
        let Some(alias) = self.ssh_alias() else {
            self.stop_ports();
            return;
        };
        // Reconnect: ControlMaster may have been renewed, set up the forwards again.
        for f in self.ports.forwards.clone() {
            let a = alias.clone();
            cx.background_executor().spawn(async move { host::ssh::forward_port(&a, f.local, f.remote, false) }).detach();
        }
        self.ports.task = Some(cx.spawn(async move |this, cx| {
            loop {
                let host = match this.update(cx, |this, _| this.remote.host.clone()) {
                    Ok(h) => h,
                    Err(_) => return,
                };
                let found = cx.background_executor().spawn(async move { ports::detect(host.as_ref()) }).await;
                if this.update(cx, |this, cx| this.on_ports(found, cx)).is_err() {
                    return;
                }
                cx.background_executor().timer(SCAN_EVERY).await;
            }
        }));
    }

    pub(super) fn stop_ports(&mut self) {
        self.ports.task = None;
        self.ports.seen.clear();
        self.ports.baseline = None;
    }

    fn on_ports(&mut self, found: Vec<u16>, cx: &mut Context<Self>) {
        let auto = cx.global::<crate::settings::Settings>().ports_auto_forward;
        match &self.ports.baseline {
            // First scan: ports already open (database, ssh) are the baseline and never auto-forwarded.
            None => self.ports.baseline = Some(found.clone()),
            Some(base) => {
                let new: Vec<u16> = found
                    .iter()
                    .copied()
                    .filter(|p| !base.contains(p) && !self.ports.seen.contains(p) && ports::worth_forwarding(*p))
                    .filter(|p| !self.ports.forwards.iter().any(|f| f.remote == *p))
                    // Our own local forward ports: if the server is this machine they'd look like
                    // "new ports" and get forwarded again in a chain.
                    .filter(|p| !self.ports.local.contains(p))
                    .collect();
                if auto {
                    for p in new {
                        self.forward_port(p, cx);
                    }
                }
            }
        }
        if found != self.ports.seen {
            self.ports.seen = found;
            cx.notify();
        }
    }

    pub(super) fn forward_port(&mut self, remote: u16, cx: &mut Context<Self>) {
        let Some(alias) = self.ssh_alias() else { return };
        if self.ports.forwards.iter().any(|f| f.remote == remote) {
            return;
        }
        let local = host::ssh::free_local_port(remote);
        self.ports.local.push(local);
        let task = cx.background_executor().spawn(async move { host::ssh::forward_port(&alias, local, remote, false) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.ports.forwards.push(Forward { remote, local });
                        let msg = if local == remote {
                            crate::i18n::tf("Port {} is forwarded: localhost:{}", &[&remote, &local])
                        } else {
                            crate::i18n::tf("Port {} is forwarded to localhost:{} ({} was busy here)", &[&remote, &local, &remote])
                        };
                        this.notice = Some(Notice::Info(msg.into()));
                    }
                    Err(e) => this.notice = Some(Notice::Error(crate::i18n::tf("Could not forward port {}: {}", &[&remote, &e]).into())),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn stop_forward(&mut self, remote: u16, cx: &mut Context<Self>) {
        let Some(i) = self.ports.forwards.iter().position(|f| f.remote == remote) else { return };
        let f = self.ports.forwards.remove(i);
        // A port closed on purpose must stay closed.
        if let Some(base) = &mut self.ports.baseline {
            base.push(remote);
        }
        if let Some(alias) = self.ssh_alias() {
            cx.background_executor().spawn(async move { host::ssh::forward_port(&alias, f.local, f.remote, true) }).detach();
        }
        cx.notify();
    }

    /// On exit: drop the forwards (ControlMaster lives 10 min; don't leave ports dangling).
    pub(super) fn cancel_all_forwards(&mut self) {
        if let Some(alias) = self.ssh_alias() {
            for f in self.ports.forwards.drain(..) {
                let _ = host::ssh::forward_port(&alias, f.local, f.remote, true);
            }
        }
    }

    /// Whether the first scan is done (baseline known; ports opened afterwards are forwarded).
    #[cfg(test)]
    pub fn ports_ready(&self) -> bool {
        self.ssh_alias().is_some() && self.ports.baseline.is_some()
    }

    #[cfg(test)]
    pub fn forwards(&self) -> Vec<Forward> {
        self.ports.forwards.clone()
    }

    pub(super) fn ports_view(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        if self.ssh_alias().is_none() {
            return center(p).text_color(c(p.text_muted)).child(crate::i18n::t("Port forwarding works while connected to an SSH host.")).into_any_element();
        }
        let button = |id: SharedString, label: &'static str, primary: bool| {
            div()
                .id(id)
                .h(px(22.))
                .px(px(theme::SPACE[3]))
                .flex()
                .items_center()
                .rounded(px(theme::radius::SM))
                .cursor_pointer()
                .when(primary, |d| d.bg(c(p.accent)).text_color(c(p.on_accent)))
                .when(!primary, |d| d.bg(c(p.surface_hover)).text_color(c(p.text)))
                .hover(|d| d.opacity(0.9))
                .child(crate::i18n::t(label))
        };
        let row = || div().h(px(layout::ROW_H + 6.)).flex().items_center().gap(px(theme::SPACE[4])).px(px(theme::SPACE[6]));
        let mut rows: Vec<AnyElement> = Vec::new();
        for f in &self.ports.forwards {
            let (remote, local) = (f.remote, f.local);
            rows.push(
                row()
                    .child(icon("plug", 14., p.success))
                    .child(div().w(px(64.)).font_family(ui::MONO_FONT).child(remote.to_string()))
                    .child(div().text_color(c(p.text_muted)).child("→"))
                    .child(div().flex_1().font_family(ui::MONO_FONT).text_color(c(p.accent)).child(format!("localhost:{local}"))) // i18n-keep: host name and port, not prose
                    .child(button(format!("port-open-{remote}").into(), crate::i18n::tr("Open in Browser"), true).on_click(cx.listener(move |_, _, _, cx| {
                        cx.open_url(&format!("http://localhost:{local}"));
                    })))
                    .child(button(format!("port-stop-{remote}").into(), crate::i18n::tr("Stop"), false).on_click(cx.listener(move |this, _, _, cx| {
                        this.stop_forward(remote, cx);
                    })))
                    .into_any_element(),
            );
        }
        let listening: Vec<u16> = self.ports.seen.iter().copied().filter(|p| !self.ports.forwards.iter().any(|f| f.remote == *p)).collect();
        for port in listening {
            rows.push(
                row()
                    .child(icon("server", 14., p.text_muted))
                    .child(div().w(px(64.)).font_family(ui::MONO_FONT).child(port.to_string()))
                    .child(div().flex_1().text_color(c(p.text_muted)).child(crate::i18n::t("listening on the server")))
                    .child(button(format!("port-fwd-{port}").into(), crate::i18n::tr("Forward Port"), false).on_click(cx.listener(move |this, _, _, cx| {
                        this.forward_port(port, cx);
                    })))
                    .into_any_element(),
            );
        }
        if rows.is_empty() {
            rows.push(row().text_color(c(p.text_muted)).child(crate::i18n::t("Nothing is listening on the server yet.")).into_any_element());
        }
        div()
            .id("ports")
            .size_full()
            .overflow_y_scroll()
            .text_size(px(text::SM.0))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                row()
                    .child(div().text_color(c(p.text_muted)).child(crate::i18n::t("Forward a port:")))
                    .child(ui::input_box("ports-field-box", p, p.bg, false).w(px(120.)).child(self.ports.field.clone())),
            )
            .children(rows)
            .into_any_element()
    }
}
