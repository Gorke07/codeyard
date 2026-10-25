//! Workbench: rendering: the update dialog and its status bar item. What the dialog says is decided in `workbench_update.rs`
//! (`dialog_model`); this file only draws it.

use gpui::{Animation, AnimationExt as _, ElementId, relative};

use super::update_glue::{Body, DialogButton, DialogModel, MAX_BUTTONS, StatusItem, Tone};
use super::*;
use crate::ui::ca;

/// Width of the card: the release notes need room, the other states do not.
const NARROW_W: f32 = 440.;
const WIDE_W: f32 = 560.;
const NOTES_MAX_H: f32 = 260.;
const ICON_BOX: f32 = 36.;
const BAR_H: f32 = 6.;

fn tone_color(tone: Tone, p: &Palette) -> u32 {
    match tone {
        Tone::Accent => p.accent,
        Tone::Success => p.success,
        Tone::Danger => p.danger,
    }
}

/// Spinner for `loader`, otherwise the named icon.
fn glyph(name: &'static str, size: f32, color: u32, id: &'static str) -> AnyElement {
    if name == "loader" { ui::spinner(id, size, color).into_any_element() } else { icon(name, size, color).into_any_element() }
}

impl Workbench {
    /// The modal card with a scrim behind it, or `None` while no dialog is open.
    pub(super) fn update_dialog(&self, p: &Palette, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let model = self.dialog_model(cx)?;
        let wide = matches!(model.body, Body::Notes(_));
        let tone = tone_color(model.tone, p);
        let header = div()
            .flex()
            .items_start()
            .gap(px(theme::SPACE[6]))
            .child(
                div()
                    .size(px(ICON_BOX))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(c(ui::mix(p.surface_raised, tone, 0.18)))
                    .child(glyph(model.icon, 18., tone, "update-dialog-spinner")),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(theme::SPACE[1]))
                    .child(
                        div()
                            .id("update-title")
                            .text_size(px(text::LG.0))
                            .line_height(px(text::LG.1))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(model.title.clone()),
                    )
                    .when_some(model.subtitle.clone(), |d, s| d.child(div().text_size(px(text::SM.0)).text_color(c(p.text_muted)).child(s))),
            );
        let card = div()
            .id("update-dialog")
            .key_context("UpdateDialog")
            .w(px(if wide { WIDE_W } else { NARROW_W }))
            .max_w(relative(0.92))
            .max_h(relative(0.9))
            .flex()
            .flex_col()
            .gap(px(theme::SPACE[7]))
            .p(px(theme::SPACE[8]))
            .bg(c(p.surface_raised))
            .border_1()
            .border_color(c(p.border))
            .rounded(px(theme::radius::LG))
            .shadow_lg()
            .text_size(px(text::BASE.0))
            .text_color(c(p.text))
            .occlude()
            .on_action(cx.listener(|this, _: &super::UpdateDismiss, window, cx| this.dismiss_update_dialog(window, cx)))
            .on_action(cx.listener(|this, _: &super::UpdateConfirm, window, cx| this.confirm_update_dialog(window, cx)))
            .on_action(cx.listener(|_, _: &super::UpdateFocusNext, window, _| window.focus_next()))
            .on_action(cx.listener(|_, _: &super::UpdateFocusPrev, window, _| window.focus_prev()))
            // A click on the text keeps the keyboard in the dialog instead of leaving it in the editor behind.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    if !this.update.focus.iter().any(|h| h.contains_focused(window, cx)) {
                        this.focus_update_primary(window, cx);
                    }
                }),
            )
            .child(header)
            .child(self.update_body(&model, p))
            .child(self.update_footer(&model, p, cx));
        Some(deferred(
            div()
                .id("update-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(ca(0x000000, 0.45))
                .occlude()
                .child(card),
        ))
    }

    fn update_body(&self, model: &DialogModel, p: &Palette) -> AnyElement {
        let muted = |d: gpui::Div| d.text_size(px(text::SM.0)).line_height(px(text::SM.1)).text_color(c(p.text_muted));
        match &model.body {
            Body::None => div().into_any_element(),
            Body::Paragraphs(lines) => div()
                .flex()
                .flex_col()
                .gap(px(theme::SPACE[4]))
                .text_size(px(text::SM.0))
                .line_height(px(text::SM.1 + 2.))
                .children(lines.iter().map(|l| div().child(l.clone())))
                .into_any_element(),
            Body::Notes(note) => div()
                .min_h_0()
                .flex()
                .flex_col()
                .gap(px(theme::SPACE[5]))
                .when_some(note.clone(), |d, n| {
                    d.child(
                        div()
                            .flex()
                            .items_start()
                            .gap(px(theme::SPACE[4]))
                            .p(px(theme::SPACE[5]))
                            .rounded(px(theme::radius::MD))
                            .bg(c(ui::mix(p.surface_raised, p.warning, 0.12)))
                            .text_size(px(text::SM.0))
                            .child(div().pt(px(2.)).child(icon("info", 14., p.warning)))
                            .child(div().flex_1().min_w_0().child(n)),
                    )
                })
                .child(
                    div()
                        .id("update-notes")
                        .min_h_0()
                        .max_h(px(NOTES_MAX_H))
                        .overflow_y_scroll()
                        .p(px(theme::SPACE[6]))
                        .rounded(px(theme::radius::MD))
                        .border_1()
                        .border_color(c(p.border))
                        .bg(c(p.bg))
                        .flex()
                        .flex_col()
                        .gap(px(theme::SPACE[3]))
                        .text_size(px(text::SM.0))
                        .children(crate::markdown::document_view(&self.update.notes, "update-notes", p)),
                )
                .into_any_element(),
            Body::Progress { fraction, caption } => {
                div().flex().flex_col().gap(px(theme::SPACE[3])).child(progress_bar(*fraction, p)).child(muted(div()).child(caption.clone())).into_any_element()
            }
            Body::Working(text) => {
                div().flex().flex_col().gap(px(theme::SPACE[3])).child(progress_bar(None, p)).child(muted(div()).child(text.clone())).into_any_element()
            }
            Body::Failure { lead, detail } => div()
                .flex()
                .flex_col()
                .gap(px(theme::SPACE[5]))
                .child(div().text_size(px(text::SM.0)).line_height(px(text::SM.1 + 2.)).child(lead.clone()))
                .child(
                    div()
                        .p(px(theme::SPACE[4]))
                        .rounded(px(theme::radius::SM))
                        .border_1()
                        .border_color(c(p.border))
                        .bg(c(p.bg))
                        .font_family(ui::MONO_FONT)
                        .text_size(px(text::XS.0))
                        .text_color(c(p.text_muted))
                        .child(detail.clone()),
                )
                .into_any_element(),
        }
    }

    fn update_footer(&self, model: &DialogModel, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let draw = |ix: usize, b: &DialogButton, cx: &mut Context<Self>| {
            let action = b.action;
            update_button(ix, b, self.update.focus.get(ix), p).on_click(cx.listener(move |this, _, window, cx| this.update_action(action, window, cx)))
        };
        let start = model.buttons.iter().enumerate().filter(|(_, b)| b.start).map(|(i, b)| draw(i, b, cx)).collect::<Vec<_>>();
        let end = model.buttons.iter().enumerate().filter(|(_, b)| !b.start).map(|(i, b)| draw(i, b, cx)).collect::<Vec<_>>();
        div()
            .flex()
            .items_center()
            .gap(px(theme::SPACE[2]))
            .children(start)
            .child(div().flex_1())
            .child(div().flex().items_center().gap(px(theme::SPACE[2])).children(end))
    }

    /// The status bar item: click opens the dialog.
    pub(super) fn update_status_button(&self, item: StatusItem, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let color = tone_color(item.tone, p);
        let tip = item.tip.clone();
        div()
            .id("sb-update")
            .h_full()
            .px(px(theme::SPACE[4]))
            .flex()
            .items_center()
            .gap(px(theme::SPACE[2]))
            .cursor_pointer()
            .hover(|d| d.bg(c(p.surface_hover)))
            .text_color(c(color))
            .child(glyph(item.icon, 14., color, "sb-update-spinner"))
            .child(item.label)
            .tooltip(move |_, cx| cx.new(|_| Tooltip(tip.clone())).into())
            .on_click(cx.listener(|this, _, window, cx| this.open_update_dialog(window, cx)))
    }
}

/// A button the keyboard can reach: the outer element holds the focus and draws the ring (a ring in the accent colour would vanish
/// on the primary button's own background, so it sits outside it).
fn update_button(ix: usize, b: &DialogButton, handle: Option<&FocusHandle>, p: &Palette) -> gpui::Stateful<gpui::Div> {
    debug_assert!(ix < MAX_BUTTONS);
    let ring = div().id(ElementId::Integer(ix as u64 + 1000)).p(px(2.)).rounded(px(theme::radius::MD)).border_2().border_color(gpui::transparent_black());
    let ring = match handle {
        Some(h) => ring.track_focus(h).focus(|s| s.border_color(c(p.focus_ring))),
        None => ring,
    };
    let (bg, fg) = if b.primary {
        (Some(p.accent), p.on_accent)
    } else if b.start {
        (None, p.text_muted)
    } else {
        (Some(p.surface_hover), p.text)
    };
    ring.cursor_pointer().child(
        div()
            .h(px(26.))
            .px(px(theme::SPACE[5]))
            .flex()
            .items_center()
            .rounded(px(theme::radius::SM))
            .text_size(px(text::SM.0))
            .text_color(c(fg))
            .when(b.primary, |d| d.font_weight(gpui::FontWeight::MEDIUM))
            .when_some(bg, |d, bg| d.bg(c(bg)))
            .when(bg.is_none(), |d| d.hover(|d| d.bg(c(p.surface_hover))))
            .when(bg.is_some(), |d| d.hover(|d| d.opacity(0.9)))
            .child(b.label.clone()),
    )
}

/// A bar that fills as `fraction` grows; without a fraction a short piece slides back and forth.
fn progress_bar(fraction: Option<f32>, p: &Palette) -> impl IntoElement + use<> {
    let track = div().h(px(BAR_H)).w_full().rounded_full().overflow_hidden().bg(c(ui::mix(p.surface_raised, p.text_muted, 0.25)));
    match fraction {
        Some(f) => track.child(div().h_full().w(relative(f.clamp(0., 1.))).rounded_full().bg(c(p.accent))).into_any_element(),
        None => track
            .child(div().h_full().w(relative(0.3)).rounded_full().bg(c(p.accent)).with_animation(
                "update-indeterminate",
                Animation::new(std::time::Duration::from_millis(1400)).repeat(),
                |bar, t| {
                    // 0 -> 1 -> 0 over one period, so the piece turns around at the ends.
                    let x = 1. - (2. * t - 1.).abs();
                    bar.ml(relative(0.7 * x))
                },
            ))
            .into_any_element(),
    }
}
