//! Workbench: in-app updates, the part with state and behaviour. `update.rs` and `update_install.rs` know how to find, download,
//! verify and install a release; this file decides when to do it, what the dialog says and what every button does. Drawing is in
//! `workbench_view_update.rs`; the network and the installer sit behind `UpdateBackend` so tests can script them.
//!
//! The rules, in one place:
//! - `update.check` = `ask` and `manual` never contact the network by themselves; only `daily` does, once in 24 hours.
//! - A check the user asked for shows its errors; a background check logs them to stderr and stays quiet.
//! - A background check that finds a release shows one notice per version and a status bar item, never a dialog.
//! - Skipping a version silences those two for it; a manual check still offers it.
//! - Quitting during a download cancels it and removes its files; quitting during the swap asks first.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::StreamExt as _;

use super::*;
use crate::i18n::{t, tf};
use crate::markdown;
use crate::update::{ErrorKind, Policy, Progress, Release, UpdateError, should_check_now};
pub(crate) use crate::update_backend::UpdateBackend;
use crate::update_install::{InstallKind, InstallPlan};

/// Keys in the state database (machine-written values stay out of `settings.jsonc`).
const LAST_CHECK: &str = "update.last_check";
const SKIPPED: &str = "update.skipped";
const INSTALLED: &str = "update.installed";
const NOTIFIED: &str = "update.notified";
const ASK_SNOOZED_UNTIL: &str = "update.ask_snoozed_until";

/// The first-run question comes a few seconds after the window is up, and waits for a calm moment (no picker, menu or dialog open).
const ASK_AFTER: Duration = Duration::from_secs(5);
const ASK_RETRY: Duration = Duration::from_secs(3);
const ASK_ATTEMPTS: u32 = 40;
/// A daily check starts this long after the question, so it never competes with the window opening.
const CHECK_AFTER_ASK: Duration = Duration::from_secs(10);
/// A session that runs for days looks again every few hours; `should_check_now` keeps it to once in 24.
const CHECK_EVERY: Duration = Duration::from_secs(3 * 60 * 60);
/// Closing the question with Esc is not an answer: it comes back after this long, not at every start.
const ASK_SNOOZE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// Enter and Space do nothing for this long after the question appears on its own, so a key typed into the editor a moment
/// earlier cannot answer it.
const ARM_AFTER: Duration = Duration::from_millis(700);
/// How long quitting waits for the update thread to finish its cleanup.
const QUIT_GRACE: Duration = Duration::from_secs(5);
/// Buttons in the dialog at most (focus handles are made once).
pub(super) const MAX_BUTTONS: usize = 4;

/// What the updater is doing. `Release` is `Clone`, so states are copied around freely.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum UpdateState {
    Idle,
    Checking,
    /// Only after a check the user asked for; a background check that finds nothing stays `Idle`.
    UpToDate,
    Available(Release),
    Downloading {
        release: Release,
        progress: Progress,
    },
    /// Hash is verified; the new app is being put in place. Not cancellable.
    Installing {
        release: Release,
    },
    /// The new version is installed; it runs after a restart. `plan` is how to start it (`None`: unknown, Windows' setup does it).
    Ready {
        version: String,
        plan: Option<InstallPlan>,
    },
    Error(Failure),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Phase {
    Check,
    Download,
    Install,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Failure {
    pub(super) kind: ErrorKind,
    /// The error's own English text: technical, shown under the plain-language lead.
    pub(super) detail: String,
    /// The release that was being downloaded or installed (`Retry` downloads it again).
    pub(super) release: Option<Release>,
    pub(super) phase: Phase,
}

/// What is on screen. The question is separate from the update dialog because it is shown without any update in sight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Dialog {
    Closed,
    Ask,
    Update,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum UpdateAction {
    CheckDaily,
    OnlyWhenAsked,
    CancelCheck,
    CancelDownload,
    Install,
    Skip,
    ReleasePage,
    Retry,
    RestartNow,
    /// Hides the dialog; what stays depends on the state (a download keeps running).
    Close,
}

/// What ends the application.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum QuitMode {
    Quit,
    /// Start the new version once this process has exited (`None`: nothing to start, the installer does it).
    Restart(Option<InstallPlan>),
}

/// Messages from the worker thread.
pub(super) enum Msg {
    Checked(Result<Option<Release>, UpdateError>),
    Progress(Progress),
    Installing,
    Done(Result<InstallPlan, UpdateError>),
}

/// The worker thread of one operation. Dropping it cancels the transfer and stops listening to it.
pub(super) struct Run {
    id: u64,
    cancel: Arc<AtomicBool>,
    exited: Arc<AtomicBool>,
    _task: gpui::Task<()>,
}

impl Drop for Run {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

// --- What the dialog shows (state in, text out; the view only draws it) ---

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tone {
    Accent,
    Success,
    Danger,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Body {
    None,
    Paragraphs(Vec<SharedString>),
    /// Release notes, with an optional line above them.
    Notes(Option<SharedString>),
    /// `fraction`: how far along (`None`: not known yet).
    Progress {
        fraction: Option<f32>,
        caption: SharedString,
    },
    Working(SharedString),
    Failure {
        lead: SharedString,
        detail: SharedString,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct DialogButton {
    pub(super) action: UpdateAction,
    pub(super) label: SharedString,
    pub(super) primary: bool,
    /// Drawn in the group on the left, away from the answer buttons.
    pub(super) start: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct DialogModel {
    /// Icon name; `loader` is drawn as a spinner.
    pub(super) icon: &'static str,
    pub(super) tone: Tone,
    pub(super) title: SharedString,
    pub(super) subtitle: Option<SharedString>,
    pub(super) body: Body,
    /// In tab order.
    pub(super) buttons: Vec<DialogButton>,
    /// What Esc does.
    pub(super) escape: UpdateAction,
}

impl DialogModel {
    /// Title and text in reading order, for the accessibility tree.
    pub(super) fn text(&self) -> String {
        let mut parts: Vec<String> = vec![self.title.to_string()];
        parts.extend(self.subtitle.iter().map(|s| s.to_string()));
        match &self.body {
            Body::None | Body::Notes(None) => {}
            Body::Notes(Some(note)) => parts.push(note.to_string()),
            Body::Paragraphs(lines) => parts.extend(lines.iter().map(|l| l.to_string())),
            Body::Progress { caption, .. } => parts.push(caption.to_string()),
            Body::Working(text) => parts.push(text.to_string()),
            Body::Failure { lead, detail } => parts.extend([lead.to_string(), detail.to_string()]),
        }
        parts.join("\n")
    }
}

/// The status bar item.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct StatusItem {
    pub(super) icon: &'static str,
    pub(super) tone: Tone,
    pub(super) label: SharedString,
    pub(super) tip: SharedString,
}

fn button(action: UpdateAction, label: &str, primary: bool) -> DialogButton {
    DialogButton { action, label: label.to_string().into(), primary, start: false }
}

fn start_button(action: UpdateAction, label: &str) -> DialogButton {
    DialogButton { action, label: label.to_string().into(), primary: false, start: true }
}

/// "12.3" for a size in megabytes (1 MB = 2^20 bytes, as file managers show it).
fn megabytes(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / (1u64 << 20) as f64)
}

/// The Releases page of GitHub for `version`, or the list when no version is known.
pub(super) fn release_url(version: Option<&str>) -> String {
    let repository = env!("CARGO_PKG_REPOSITORY");
    match version {
        Some(v) => format!("{repository}/releases/tag/v{v}"),
        None => format!("{repository}/releases/latest"),
    }
}

impl Failure {
    pub(super) fn title(&self) -> String {
        match (self.kind, self.phase) {
            (ErrorKind::Network, Phase::Check) => t("Couldn't check for updates"),
            (ErrorKind::Network, _) => t("Couldn't download the update"),
            (ErrorKind::Untrusted, _) => t("The update couldn't be verified"),
            _ => t("The update couldn't be installed"),
        }
        .to_string()
    }

    fn lead(&self) -> String {
        match self.kind {
            ErrorKind::Network => t("Codeyard couldn't reach the update server. Check your internet connection and try again."),
            ErrorKind::Untrusted => t("Nothing was installed. What the update server sent didn't match the signed checksums, so Codeyard discarded it."),
            _ => t("The new version couldn't be installed from here. You can download it from the release page instead."),
        }
        .to_string()
    }

    fn from_error(e: &UpdateError, release: Option<Release>, phase: Phase) -> Self {
        Failure { kind: e.kind(), detail: e.to_string(), release, phase }
    }
}

impl UpdateState {
    /// Something is running on a worker thread right now.
    pub(super) fn is_working(&self) -> bool {
        matches!(self, UpdateState::Checking | UpdateState::Downloading { .. } | UpdateState::Installing { .. })
    }

    fn version(&self) -> Option<&str> {
        match self {
            UpdateState::Available(r) | UpdateState::Downloading { release: r, .. } | UpdateState::Installing { release: r } => Some(&r.version),
            UpdateState::Ready { version, .. } => Some(version),
            UpdateState::Error(f) => f.release.as_ref().map(|r| r.version.as_str()),
            _ => None,
        }
    }
}

fn progress_fraction(p: &Progress) -> Option<f32> {
    p.total.filter(|t| *t > 0).map(|t| (p.done as f64 / t as f64).clamp(0., 1.) as f32)
}

impl Workbench {
    /// What the dialog shows for the current state, or `None` while it is closed.
    pub(super) fn dialog_model(&self, cx: &App) -> Option<DialogModel> {
        use UpdateAction as A;
        let version = self.update.backend.version().to_string();
        match (self.update.dialog, &self.update.state) {
            (Dialog::Closed, _) => None,
            (Dialog::Ask, _) => Some(DialogModel {
                icon: "refresh-cw",
                tone: Tone::Accent,
                title: t("Check for updates automatically?").into(),
                subtitle: None,
                body: Body::Paragraphs(vec![
                    t("Once a day, Codeyard can ask github.com for the checksum list of the latest release to see whether a newer version exists. That request is all that is sent: nothing about you or your files, no account and no tracking.").into(),
                    t("You can change this at any time in Settings (update.check). Check for Updates… always works, whatever you choose.").into(),
                ]),
                buttons: vec![button(A::OnlyWhenAsked, t("Only When I Ask"), false), button(A::CheckDaily, t("Check Daily"), true)],
                escape: A::Close,
            }),
            (Dialog::Update, UpdateState::Idle) => None,
            (Dialog::Update, UpdateState::Checking) => Some(DialogModel {
                icon: "loader",
                tone: Tone::Accent,
                title: t("Checking for updates…").into(),
                subtitle: None,
                body: Body::None,
                buttons: vec![button(A::CancelCheck, t("Cancel"), true)],
                escape: A::CancelCheck,
            }),
            (Dialog::Update, UpdateState::UpToDate) => Some(DialogModel {
                icon: "check",
                tone: Tone::Success,
                title: t("Codeyard is up to date").into(),
                subtitle: Some(tf("You have {}, the latest version.", &[&version]).into()),
                body: Body::None,
                buttons: vec![button(A::Close, t("Close"), true)],
                escape: A::Close,
            }),
            (Dialog::Update, UpdateState::Available(release)) => {
                let can_install = !matches!(self.update.kind, InstallKind::Manual) && release.asset.is_some();
                let note = match (&self.update.kind, release.asset.is_some()) {
                    (InstallKind::Manual, _) => Some(t("This copy isn't installed as an app, so download the new version yourself.").into()),
                    (_, false) => Some(t("This release has no installer for this system. Download it from the release page.").into()),
                    _ => None,
                };
                let mut buttons = vec![start_button(A::Skip, t("Skip This Version"))];
                if can_install {
                    buttons.push(start_button(A::ReleasePage, t("Release Page")));
                }
                buttons.push(button(A::Close, t("Later"), false));
                buttons.push(if can_install {
                    button(A::Install, t("Install and Restart"), true)
                } else {
                    button(A::ReleasePage, t("Download Page"), true)
                });
                Some(DialogModel {
                    icon: "arrow-down",
                    tone: Tone::Accent,
                    title: tf("Codeyard {} is available", &[&release.version]).into(),
                    subtitle: Some(tf("You have {}", &[&version]).into()),
                    body: Body::Notes(note),
                    buttons,
                    escape: A::Close,
                })
            }
            (Dialog::Update, UpdateState::Downloading { release, progress }) => {
                let cancelling = self.update.run.as_ref().is_some_and(|r| r.cancel.load(Ordering::Relaxed));
                let caption = match (cancelling, progress.total) {
                    (true, _) => t("Cancelling…").to_string(),
                    (_, Some(total)) => tf("{0} of {1} MB", &[&megabytes(progress.done), &megabytes(total)]),
                    (_, None) if progress.done == 0 => t("Starting the download…").to_string(),
                    (_, None) => format!("{} MB", megabytes(progress.done)),
                };
                Some(DialogModel {
                    icon: "arrow-down",
                    tone: Tone::Accent,
                    title: tf("Downloading Codeyard {}", &[&release.version]).into(),
                    subtitle: None,
                    body: Body::Progress { fraction: progress_fraction(progress), caption: caption.into() },
                    buttons: vec![button(A::CancelDownload, t("Cancel"), false), button(A::Close, t("Hide"), true)],
                    escape: A::Close,
                })
            }
            (Dialog::Update, UpdateState::Installing { release }) => Some(DialogModel {
                icon: "loader",
                tone: Tone::Accent,
                title: tf("Installing Codeyard {}", &[&release.version]).into(),
                subtitle: None,
                body: Body::Working(
                    if matches!(self.update.kind, InstallKind::WindowsInstalled) { t("Installing — Codeyard will restart.") } else { t("Verifying and installing…") }.into(),
                ),
                buttons: vec![button(A::Close, t("Hide"), true)],
                escape: A::Close,
            }),
            (Dialog::Update, UpdateState::Ready { .. }) => {
                let mut paragraphs: Vec<SharedString> = vec![t("Restart Codeyard to finish.").into()];
                if cx.global::<Settings>().hot_exit {
                    paragraphs.push(t("Your open files and unsaved changes come back after the restart.").into());
                }
                Some(DialogModel {
                    icon: "check",
                    tone: Tone::Success,
                    title: t("Update installed").into(),
                    subtitle: None,
                    body: Body::Paragraphs(paragraphs),
                    buttons: vec![button(A::Close, t("Later"), false), button(A::RestartNow, t("Restart Now"), true)],
                    escape: A::Close,
                })
            }
            (Dialog::Update, UpdateState::Error(f)) => {
                let mut buttons = Vec::new();
                match f.kind {
                    ErrorKind::Network => {
                        buttons.push(start_button(A::ReleasePage, t("Release Page")));
                        buttons.push(button(A::Close, t("Close"), false));
                        buttons.push(button(A::Retry, t("Try Again"), true));
                    }
                    _ => {
                        buttons.push(button(A::Close, t("Close"), false));
                        buttons.push(button(A::ReleasePage, t("Release Page"), true));
                    }
                }
                Some(DialogModel {
                    icon: "alert",
                    tone: Tone::Danger,
                    title: f.title().into(),
                    subtitle: None,
                    body: Body::Failure { lead: f.lead().into(), detail: f.detail.clone().into() },
                    buttons,
                    escape: A::Close,
                })
            }
        }
    }

    /// The status bar item: shown while an update is on offer, on its way or waiting for a restart.
    pub(super) fn update_status_item(&self) -> Option<StatusItem> {
        let version = self.update.state.version()?.to_string();
        let (icon, tone, label, tip) = match &self.update.state {
            UpdateState::Available(_) => ("arrow-down", Tone::Accent, version.clone(), tf("Codeyard {} is available. Click for details.", &[&version])),
            UpdateState::Downloading { progress, .. } => {
                let label = match progress_fraction(progress) {
                    Some(f) => tf("Updating… {}%", &[&((f * 100.).floor() as u32)]),
                    None => t("Updating…").to_string(),
                };
                ("loader", Tone::Accent, label, tf("Downloading Codeyard {}. Click for details.", &[&version]))
            }
            UpdateState::Installing { .. } => {
                ("loader", Tone::Accent, t("Installing…").to_string(), tf("Installing Codeyard {}. Click for details.", &[&version]))
            }
            UpdateState::Ready { .. } => {
                ("refresh-cw", Tone::Success, t("Restart to update").to_string(), tf("Codeyard {} is installed. Click for details.", &[&version]))
            }
            UpdateState::Error(_) => ("alert", Tone::Danger, t("Update failed").to_string(), t("The update failed. Click for details.").to_string()),
            _ => return None,
        };
        Some(StatusItem { icon, tone, label: label.into(), tip: tip.into() })
    }

    // --- Commands and buttons ---

    /// "Check for Updates…": works whatever `update.check` says.
    pub(super) fn check_for_updates(&mut self, _: &super::CheckForUpdates, window: &mut Window, cx: &mut Context<Self>) {
        self.begin_check(true, window, cx);
    }

    pub(super) fn update_action(&mut self, action: UpdateAction, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            UpdateAction::CheckDaily => self.choose_policy(Policy::Daily, window, cx),
            UpdateAction::OnlyWhenAsked => self.choose_policy(Policy::Manual, window, cx),
            UpdateAction::CancelCheck => self.cancel_check(window, cx),
            UpdateAction::CancelDownload => self.cancel_download(cx),
            UpdateAction::Install => self.start_update_download(window, cx),
            UpdateAction::Skip => self.skip_version(window, cx),
            UpdateAction::ReleasePage => {
                let version = self.update.state.version().map(str::to_string);
                cx.open_url(&release_url(version.as_deref()));
            }
            UpdateAction::Retry => self.retry(window, cx),
            UpdateAction::RestartNow => self.restart_now(window, cx),
            UpdateAction::Close => self.close_update_dialog(window, cx),
        }
    }

    /// Esc.
    pub(super) fn dismiss_update_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(model) = self.dialog_model(cx) {
            self.update_action(model.escape, window, cx);
        }
    }

    /// Enter and Space: the focused button, or the primary one when focus is elsewhere in the dialog.
    pub(super) fn confirm_update_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.update.dialog == Dialog::Ask && !self.update.armed {
            return;
        }
        let Some(model) = self.dialog_model(cx) else { return };
        let focused = self.update.focus.iter().position(|h| h.is_focused(window)).filter(|i| *i < model.buttons.len());
        let chosen = focused.map(|i| &model.buttons[i]).or_else(|| model.buttons.iter().find(|b| b.primary));
        if let Some(b) = chosen {
            self.update_action(b.action, window, cx);
        }
    }

    pub(super) fn open_update_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.update.dialog == Dialog::Closed {
            self.update.return_focus = window.focused(cx);
        }
        self.update.dialog = Dialog::Update;
        self.focus_update_primary(window, cx);
        cx.notify();
    }

    fn close_update_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.update.dialog {
            Dialog::Closed => return,
            // Not answering is not an answer: stay undecided, and ask again later rather than at every start.
            Dialog::Ask => {
                let until = self.update.backend.now() + ASK_SNOOZE;
                self.remember(ASK_SNOOZED_UNTIL, &unix_ms(until).to_string());
            }
            Dialog::Update => {}
        }
        self.update.dialog = Dialog::Closed;
        // What was only worth showing in the dialog is dropped with it; a failed update keeps its status item.
        match &self.update.state {
            UpdateState::UpToDate => self.update.state = UpdateState::Idle,
            UpdateState::Error(f) if f.release.is_none() => self.update.state = UpdateState::Idle,
            _ => {}
        }
        match self.update.return_focus.take() {
            Some(handle) => window.focus(&handle),
            None => window.focus(&self.focus_handle),
        }
        cx.notify();
    }

    /// Focus goes to the primary button, so Enter does what the dialog recommends.
    pub(super) fn focus_update_primary(&mut self, window: &mut Window, cx: &App) {
        let Some(model) = self.dialog_model(cx) else { return };
        let ix = model.buttons.iter().position(|b| b.primary).unwrap_or(0);
        if let Some(handle) = self.update.focus.get(ix) {
            window.focus(handle);
        }
    }

    // --- The question ---

    /// Shows the one-time question when `update.check` is undecided, it was not snoozed and nothing else is in the way (a picker or
    /// menu is open, or the window is in the background: a dialog must not appear under someone else's hands). `false`: not now,
    /// try again soon.
    pub(super) fn ask_update_policy(&mut self, window_active: bool, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let snoozed = self.stored(ASK_SNOOZED_UNTIL).and_then(|v| v.parse::<u64>().ok()).is_some_and(|until| unix_ms(self.update.backend.now()) < until);
        if cx.global::<Settings>().update_check != Policy::Ask || snoozed {
            return true;
        }
        let calm = self.overlay.is_none() && self.menu.is_none() && self.update.dialog == Dialog::Closed && window_active;
        if !calm {
            return false;
        }
        self.update.return_focus = window.focused(cx);
        self.update.dialog = Dialog::Ask;
        self.update.armed = false;
        self.focus_update_primary(window, cx);
        self.update.arm = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(ARM_AFTER).await;
            this.update(cx, |this, cx| {
                this.update.armed = true;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
        true
    }

    fn choose_policy(&mut self, policy: Policy, window: &mut Window, cx: &mut Context<Self>) {
        self.update.arm = None;
        // The dialog closes first so the notice that `write_setting` leaves ("update.check: daily") is the last word.
        self.update.dialog = Dialog::Closed;
        match self.update.return_focus.take() {
            Some(handle) => window.focus(&handle),
            None => window.focus(&self.focus_handle),
        }
        self.write_setting("update.check", serde_json::json!(policy.as_str()), window, cx);
        // Saying yes to daily checks is also the first one: nobody wants to wait a day to see it work.
        if policy == Policy::Daily && cx.global::<Settings>().update_check == Policy::Daily {
            self.begin_check(false, window, cx);
        }
    }

    // --- Scheduling ---

    /// First window only, from `main`: the question (if undecided) and the daily check (if chosen). Never called in debug builds,
    /// in tests or with `--do`/`--panel`.
    pub(crate) fn schedule_update_checks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.update.schedule = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(ASK_AFTER).await;
            for _ in 0..ASK_ATTEMPTS {
                match this.update_in(cx, |this, window, cx| this.ask_update_policy(window.is_window_active(), window, cx)) {
                    Ok(true) => break,
                    Ok(false) => cx.background_executor().timer(ASK_RETRY).await,
                    Err(_) => return,
                }
            }
            cx.background_executor().timer(CHECK_AFTER_ASK).await;
            loop {
                if this.update_in(cx, |this, window, cx| this.check_if_due(window, cx)).is_err() {
                    return;
                }
                cx.background_executor().timer(CHECK_EVERY).await;
            }
        }));
    }

    /// The background check of policy `daily`: only when 24 hours passed since the last good one.
    pub(super) fn check_if_due(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let policy = cx.global::<Settings>().update_check;
        if should_check_now(self.last_check(), self.update.backend.now(), policy) {
            self.begin_check(false, window, cx);
        }
    }

    // --- Checking ---

    /// `manual`: the user asked, so the dialog opens and errors are shown. Otherwise nothing is shown unless a release is found.
    pub(super) fn begin_check(&mut self, manual: bool, window: &mut Window, cx: &mut Context<Self>) {
        if manual && let Some(other) = self.window_updating(window, cx) {
            // One update at a time: the window that is already doing it shows its dialog.
            other
                .update(cx, |wb, window, cx| {
                    window.activate_window();
                    wb.open_update_dialog(window, cx);
                })
                .ok();
            return;
        }
        match &self.update.state {
            UpdateState::Downloading { .. } | UpdateState::Installing { .. } | UpdateState::Ready { .. } => {
                if manual {
                    self.open_update_dialog(window, cx);
                }
                return;
            }
            UpdateState::Checking => {
                // Adopt the check that is already running.
                if manual {
                    self.update.manual = true;
                    self.open_update_dialog(window, cx);
                }
                return;
            }
            // A background check only looks when nothing is on offer or on screen.
            UpdateState::Available(_) | UpdateState::UpToDate | UpdateState::Error(_) if !manual => return,
            _ => {}
        }
        self.update.state = UpdateState::Checking;
        self.update.manual = manual;
        if manual {
            self.open_update_dialog(window, cx);
        }
        let started = self.start_worker(
            |backend, tx, _| {
                // A closed channel means the window is gone or the check was cancelled: nobody wants the answer.
                let _ = tx.send(Msg::Checked(backend.check()));
            },
            window,
            cx,
        );
        if let Err(e) = started {
            self.fail(&e, None, Phase::Check, window, cx);
        }
        cx.notify();
    }

    /// Another window that is checking, downloading or installing right now.
    fn window_updating(&self, window: &Window, cx: &App) -> Option<gpui::WindowHandle<Workbench>> {
        let me = window.window_handle();
        cx.windows()
            .into_iter()
            .filter(|w| *w != me)
            .filter_map(|w| w.downcast::<Workbench>())
            .find(|w| w.read(cx).is_ok_and(|wb| wb.update.state.is_working()))
    }

    fn cancel_check(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The request itself cannot be interrupted; its answer is dropped with the run.
        self.update.run = None;
        self.update.manual = false;
        self.update.state = UpdateState::Idle;
        self.close_update_dialog(window, cx);
    }

    fn on_checked(&mut self, result: Result<Option<Release>, UpdateError>, window: &mut Window, cx: &mut Context<Self>) {
        self.update.run = None;
        let manual = std::mem::take(&mut self.update.manual);
        match result {
            Ok(None) => {
                self.record_check();
                self.update.state = if manual { UpdateState::UpToDate } else { UpdateState::Idle };
            }
            Ok(Some(release)) => {
                self.record_check();
                self.offer(release, manual, cx);
            }
            Err(e) => self.fail(&e, None, Phase::Check, window, cx),
        }
        if manual {
            self.focus_update_primary(window, cx);
        }
        cx.notify();
    }

    /// A newer release exists. Quiet paths (background check): an installed one waits for a restart, a skipped one is not mentioned.
    fn offer(&mut self, release: Release, manual: bool, cx: &mut Context<Self>) {
        let version = release.version.clone();
        if self.stored(INSTALLED).as_deref() == Some(version.as_str()) {
            let plan = self.plan_for_restart();
            self.update.state = UpdateState::Ready { version, plan };
            return;
        }
        if !manual && self.stored(SKIPPED).as_deref() == Some(version.as_str()) {
            self.update.state = UpdateState::Idle;
            return;
        }
        self.update.kind = self.update.backend.install_kind();
        self.update.notes = markdown::parse(&release.notes);
        self.update.state = UpdateState::Available(release);
        if !manual && self.stored(NOTIFIED).as_deref() != Some(version.as_str()) {
            self.remember(NOTIFIED, &version);
            self.notice = Some(Notice::Info(tf("Codeyard {} is available.", &[&version]).into()));
        }
        cx.notify();
    }

    fn skip_version(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let UpdateState::Available(release) = &self.update.state {
            let version = release.version.clone();
            self.remember(SKIPPED, &version);
            self.update.state = UpdateState::Idle;
        }
        self.close_update_dialog(window, cx);
    }

    // --- Downloading and installing ---

    fn start_update_download(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let release = match &self.update.state {
            UpdateState::Available(r) => r.clone(),
            UpdateState::Error(Failure { release: Some(r), .. }) => r.clone(),
            _ => return,
        };
        if matches!(self.update.kind, InstallKind::Manual) || release.asset.is_none() {
            return;
        }
        let kind = self.update.kind.clone();
        self.update.state = UpdateState::Downloading { release: release.clone(), progress: Progress { done: 0, total: None } };
        let started = self.start_worker(
            move |backend, tx, cancel| {
                // Same as for the check: a closed channel only means nobody is listening any more.
                let result = (|| {
                    let dl = backend.download(&release, &mut |p| drop(tx.send(Msg::Progress(p))), cancel)?;
                    if cancel.load(Ordering::Relaxed) {
                        backend.discard(dl);
                        return Err(UpdateError::Cancelled);
                    }
                    let _ = tx.send(Msg::Installing);
                    let installed = backend.stage_and_install(&dl, &kind);
                    // The Windows setup program runs from this file; everything else is done with it.
                    if !matches!(installed, Ok(InstallPlan::WindowsInstaller)) {
                        backend.discard(dl);
                    }
                    installed
                })();
                let _ = tx.send(Msg::Done(result));
            },
            window,
            cx,
        );
        if let Err(e) = started {
            let release = self.update.state.release().cloned();
            self.fail(&e, release, Phase::Download, window, cx);
        }
        self.focus_update_primary(window, cx);
        cx.notify();
    }

    fn cancel_download(&mut self, cx: &mut Context<Self>) {
        // The worker notices within a moment, removes the partial file and answers `Cancelled`.
        if let Some(run) = &self.update.run
            && matches!(self.update.state, UpdateState::Downloading { .. })
        {
            run.cancel.store(true, Ordering::Relaxed);
            cx.notify();
        }
    }

    fn retry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let UpdateState::Error(failure) = &self.update.state else { return };
        match (failure.phase, failure.release.clone()) {
            (Phase::Download, Some(release)) => {
                self.update.state = UpdateState::Available(release);
                self.start_update_download(window, cx);
            }
            _ => {
                self.update.state = UpdateState::Idle;
                self.begin_check(true, window, cx);
            }
        }
    }

    fn on_installed(&mut self, result: Result<InstallPlan, UpdateError>, window: &mut Window, cx: &mut Context<Self>) {
        self.update.run = None;
        let release = self.update.state.release().cloned();
        let phase = if matches!(self.update.state, UpdateState::Installing { .. }) { Phase::Install } else { Phase::Download };
        match (result, release) {
            (Ok(plan), Some(release)) => {
                self.remember(INSTALLED, &release.version);
                self.update.state = UpdateState::Ready { version: release.version, plan: Some(plan.clone()) };
                if plan == InstallPlan::WindowsInstaller {
                    // The setup program is running and closes this app itself; leave through the normal quit path first.
                    self.restart_now(window, cx);
                } else if self.update.dialog != Dialog::Update {
                    self.notice = Some(Notice::Info(t("Update installed. Restart Codeyard to finish.").into()));
                }
            }
            (Err(e), Some(release)) if e.kind() == ErrorKind::Cancelled => self.update.state = UpdateState::Available(release),
            (Err(e), release) => self.fail(&e, release, phase, window, cx),
            (Ok(_), None) => {}
        }
        self.focus_update_primary(window, cx);
        cx.notify();
    }

    /// Turns an error into what the user sees: the dialog when it is open, otherwise (a background step) the log, and for a
    /// failed download or install also a notice, because someone was waiting for it.
    fn fail(&mut self, error: &UpdateError, release: Option<Release>, phase: Phase, window: &mut Window, cx: &mut Context<Self>) {
        self.update.run = None;
        let failure = Failure::from_error(error, release, phase);
        eprintln!("update: {}: {}", failure.title(), failure.detail);
        if self.update.dialog == Dialog::Update {
            self.update.state = UpdateState::Error(failure);
        } else if phase == Phase::Check {
            self.update.state = UpdateState::Idle;
        } else {
            self.notice = Some(Notice::Error(failure.title().into()));
            self.update.state = UpdateState::Error(failure);
        }
        self.focus_update_primary(window, cx);
        cx.notify();
    }

    // --- Worker threads ---

    /// Runs `job` on its own thread (a download can take half an hour, too long for the shared pool) and feeds its messages to
    /// `on_update_msg`. At most one run exists; starting another drops (and cancels) the previous one.
    fn start_worker(
        &mut self,
        job: impl FnOnce(&dyn UpdateBackend, &std::sync::mpsc::Sender<Msg>, &AtomicBool) + Send + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), UpdateError> {
        self.update.next_run += 1;
        let id = self.update.next_run;
        let (cancel, exited) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
        let (tx, rx) = std::sync::mpsc::channel();
        let backend = self.update.backend.clone();
        let (thread_cancel, thread_exited) = (cancel.clone(), exited.clone());
        std::thread::Builder::new()
            .name("update".into())
            .spawn(move || {
                job(&*backend, &tx, &thread_cancel);
                drop(tx);
                thread_exited.store(true, Ordering::Release);
            })
            .map_err(|source| UpdateError::Io { what: "start the update", source })?;
        let mut messages = ui::bridge(rx);
        let task = cx.spawn_in(window, async move |this, cx| {
            while let Some(msg) = messages.next().await {
                if this.update_in(cx, |this, window, cx| this.on_update_msg(id, msg, window, cx)).is_err() {
                    break;
                }
            }
        });
        self.update.run = Some(Run { id, cancel, exited, _task: task });
        Ok(())
    }

    fn on_update_msg(&mut self, id: u64, msg: Msg, window: &mut Window, cx: &mut Context<Self>) {
        // A message of a run that was cancelled or replaced.
        if self.update.run.as_ref().map(|r| r.id) != Some(id) {
            return;
        }
        match msg {
            Msg::Checked(result) => self.on_checked(result, window, cx),
            Msg::Progress(progress) => {
                if let UpdateState::Downloading { progress: shown, .. } = &mut self.update.state {
                    *shown = progress;
                    cx.notify();
                }
            }
            Msg::Installing => {
                if let UpdateState::Downloading { release, .. } = &self.update.state {
                    self.update.state = UpdateState::Installing { release: release.clone() };
                    // The button that had the focus (Cancel) is gone.
                    self.focus_update_primary(window, cx);
                    cx.notify();
                }
            }
            Msg::Done(result) => self.on_installed(result, window, cx),
        }
    }

    // --- Restart and quit ---

    pub(super) fn restart_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let plan = match &self.update.state {
            UpdateState::Ready { plan: Some(plan), .. } => Some(plan.clone()),
            UpdateState::Ready { plan: None, .. } => self.plan_for_restart(),
            _ => return,
        };
        self.confirm_quit_with(QuitMode::Restart(plan), window, cx);
    }

    /// How to start the installed version when this window did not do the installing itself.
    fn plan_for_restart(&self) -> Option<InstallPlan> {
        match self.update.backend.install_kind() {
            InstallKind::MacBundle { app } => Some(InstallPlan::MacApp { app }),
            InstallKind::WindowsInstalled | InstallKind::Manual => None,
        }
    }

    /// The last step of quitting or restarting, after hot exit has saved everything. The new version is started BEFORE this
    /// process ends (its helper waits for us to exit); if that fails nothing is quit, so the user is not left without an editor.
    pub(super) fn finish_quit(&mut self, mode: QuitMode, cx: &mut Context<Self>) {
        if let QuitMode::Restart(Some(plan)) = &mode
            && let Err(e) = self.update.backend.relaunch(plan)
        {
            self.notice =
                Some(Notice::Error(tf("Codeyard could not start the new version: {}. The update is installed; open Codeyard again to use it.", &[&e]).into()));
            cx.notify();
            return;
        }
        let backend = self.update.backend.clone();
        backend.quit(cx);
    }

    /// While the new app is being swapped in, ending the process can leave a half-installed app: ask first.
    /// `true`: the question is on screen and the caller must stop.
    pub(super) fn hold_quit_for_install(&mut self, mode: QuitMode, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !matches!(self.update.state, UpdateState::Installing { .. }) {
            return false;
        }
        let answer = window.prompt(
            gpui::PromptLevel::Warning,
            t("Codeyard is still installing an update"),
            Some(t("Quitting now can leave the installation unfinished. It takes a few seconds.")),
            &[t("Quit Anyway"), t("Keep Waiting")],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await == Ok(0) {
                this.update(cx, |this, cx| this.finish_quit(mode, cx)).ok();
            }
        })
        .detach();
        true
    }

    /// The last window is being closed (which ends the app).
    pub(crate) fn hold_close_for_install(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.hold_quit_for_install(QuitMode::Quit, window, cx)
    }

    /// The application is quitting: a download is cancelled (its worker removes the partial file) and the returned flag says when
    /// the worker is done. A check is simply abandoned.
    pub(super) fn cancel_update_for_quit(&mut self) -> Option<Arc<AtomicBool>> {
        let run = self.update.run.take()?;
        run.cancel.store(true, Ordering::Relaxed);
        Some(run.exited.clone())
    }

    /// Waits (briefly) for `exited`, so the temporary files are gone before the process ends.
    pub(super) fn wait_for_update_thread(exited: Arc<AtomicBool>, cx: &App) -> impl std::future::Future<Output = ()> + use<> {
        let executor = cx.background_executor().clone();
        async move {
            let start = std::time::Instant::now();
            while !exited.load(Ordering::Acquire) && start.elapsed() < QUIT_GRACE {
                executor.timer(Duration::from_millis(50)).await;
            }
        }
    }

    // --- Stored values ---

    fn stored(&self, key: &str) -> Option<String> {
        self.persist.state.as_ref().and_then(|s| s.get(key))
    }

    /// A failed write only costs a repeated notice or check, so it is logged, not shown.
    fn remember(&self, key: &str, value: &str) {
        if let Some(Err(e)) = self.persist.state.as_ref().map(|s| s.set(key, value)) {
            eprintln!("could not remember {key}: {e}");
        }
    }

    fn last_check(&self) -> Option<SystemTime> {
        self.update.last_check.or_else(|| self.stored(LAST_CHECK)?.parse::<u64>().ok().map(|ms| UNIX_EPOCH + Duration::from_millis(ms)))
    }

    /// A check that got an answer (a release or "up to date") counts; a network failure does not, so the next start tries again.
    fn record_check(&mut self) {
        let now = self.update.backend.now();
        self.update.last_check = Some(now);
        self.remember(LAST_CHECK, &unix_ms(now).to_string());
    }
}

impl UpdateState {
    fn release(&self) -> Option<&Release> {
        match self {
            UpdateState::Available(r) | UpdateState::Downloading { release: r, .. } | UpdateState::Installing { release: r } => Some(r),
            UpdateState::Error(f) => f.release.as_ref(),
            _ => None,
        }
    }
}

/// Milliseconds since the Unix epoch (0 for a clock set before it).
fn unix_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_shown_in_megabytes_with_one_decimal() {
        assert_eq!(megabytes(0), "0.0");
        assert_eq!(megabytes(12_897_484), "12.3");
        assert_eq!(megabytes(20_700_000), "19.7");
    }

    #[test]
    fn the_release_page_is_the_tag_of_the_version_or_the_latest_release() {
        let repository = env!("CARGO_PKG_REPOSITORY");
        assert_eq!(release_url(Some("0.2.0")), format!("{repository}/releases/tag/v0.2.0"));
        assert_eq!(release_url(None), format!("{repository}/releases/latest"));
    }

    #[test]
    fn progress_is_a_fraction_only_when_the_total_is_known() {
        assert_eq!(progress_fraction(&Progress { done: 5, total: Some(10) }), Some(0.5));
        assert_eq!(progress_fraction(&Progress { done: 5, total: None }), None);
        assert_eq!(progress_fraction(&Progress { done: 5, total: Some(0) }), None, "no division by zero");
        assert_eq!(progress_fraction(&Progress { done: 12, total: Some(10) }), Some(1.), "a server that sends more than it said");
    }

    #[test]
    fn every_error_kind_has_a_plain_title_and_only_network_errors_are_retried() {
        let failure = |kind, phase| Failure { kind, detail: String::new(), release: None, phase };
        assert_eq!(failure(ErrorKind::Network, Phase::Check).title(), "Couldn't check for updates");
        assert_eq!(failure(ErrorKind::Network, Phase::Download).title(), "Couldn't download the update");
        assert_eq!(failure(ErrorKind::Untrusted, Phase::Download).title(), "The update couldn't be verified");
        assert_eq!(failure(ErrorKind::Install, Phase::Install).title(), "The update couldn't be installed");
        assert!(failure(ErrorKind::Untrusted, Phase::Check).lead().starts_with("Nothing was installed."));
    }
}
