//! Workbench: editor groups: split, focus, moving tabs.

use super::*;

impl Workbench {
    /// View slots of the groups: left group 0, right group 1.
    pub(super) fn focused_slot(&self) -> usize {
        if self.tabs.focused_left { 0 } else { 1 }
    }

    /// Map editors' views to groups: one view per group it's in;
    /// outside calls (and popups) get the focused group's.
    pub(super) fn sync_slots(&self, cx: &mut App) {
        let (fs, os) = (self.focused_slot(), 1 - self.focused_slot());
        let other: &[Entity<EditorView>] = self.tabs.other.as_ref().map_or(&[], |g| &g.editors);
        for ed in self.all_editors() {
            let (in_f, in_o) = (self.tabs.editors.contains(&ed), other.contains(&ed));
            ed.update(cx, |e, cx| {
                if in_f {
                    e.open_slot(fs, cx);
                }
                if in_o {
                    e.open_slot(os, cx);
                }
                if !in_f {
                    e.close_slot(fs);
                }
                if !in_o {
                    e.close_slot(os);
                }
                e.set_home(if in_f { fs } else { os });
            });
        }
    }

    /// Visible tabs (the front ones of both groups) keep a syntax tree, the rest drop it.
    pub(super) fn refresh_active_flags(&self, cx: &mut App) {
        let front: Vec<Entity<EditorView>> = self
            .tabs
            .active
            .and_then(|a| self.tabs.editors.get(a))
            .into_iter()
            .chain(self.tabs.other.as_ref().and_then(|g| g.active.and_then(|a| g.editors.get(a))))
            .cloned()
            .collect();
        for ed in self.all_editors() {
            let shown = front.contains(&ed);
            ed.update(cx, |e, _| e.set_active(shown));
        }
    }

    /// Is the editor open in the other group too (the document lives on when it's closed).
    pub(super) fn also_in_other(&self, ed: &Entity<EditorView>) -> bool {
        self.tabs.other.as_ref().is_some_and(|g| g.editors.contains(ed))
    }

    /// Give focus to the other group (the tab lists swap places).
    pub(super) fn swap_groups(&mut self) {
        let Some(g) = &mut self.tabs.other else { return };
        std::mem::swap(&mut self.tabs.editors, &mut g.editors);
        std::mem::swap(&mut self.tabs.active, &mut g.active);
        self.tabs.focused_left = !self.tabs.focused_left;
        self.tabs.diff_active = false;
    }

    /// If the clicked group (tab bar or panel) isn't focused, focus it.
    pub(super) fn ensure_group(&mut self, focused: bool) {
        if !focused {
            self.swap_groups();
        }
    }

    pub(super) fn focus_group(&mut self, left: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.other.is_some() && left != self.tabs.focused_left {
            self.swap_groups();
        }
        match self.tabs.active {
            Some(a) => self.activate(a, window, cx),
            None => window.focus(&self.focus_handle),
        }
        cx.notify();
    }

    pub(super) fn focus_left_group(&mut self, _: &FocusLeftGroup, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_group(true, window, cx);
    }

    pub(super) fn focus_right_group(&mut self, _: &FocusRightGroup, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_group(false, window, cx);
    }

    /// Put the dragged tab before the `before` tab (at the end if none); if dropped on another group,
    /// move it to that group. The dropped tab becomes active, focus goes with it.
    pub(super) fn drop_tab(
        &mut self,
        ed: Entity<EditorView>,
        from_focused: bool,
        to_focused: bool,
        before: Option<Entity<EditorView>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if Some(&ed) == before.as_ref() {
            return;
        }
        // The target group's identity changes after the swap: track it as left/right.
        let to_left = self.tabs.focused_left == to_focused;
        self.ensure_group(from_focused);
        let Some(ix) = self.tabs.editors.iter().position(|e| *e == ed) else { return };
        if from_focused != to_focused {
            self.tabs.active = Some(ix);
            self.move_editor_to_other_group(&MoveEditorToOtherGroup, window, cx);
            if self.tabs.focused_left != to_left {
                return; // couldn't move (unsplit layout with a single tab)
            }
        }
        let Some(from) = self.tabs.editors.iter().position(|e| *e == ed) else { return };
        let moving = self.tabs.editors.remove(from);
        let to = before.and_then(|b| self.tabs.editors.iter().position(|e| *e == b)).unwrap_or(self.tabs.editors.len());
        self.tabs.editors.insert(to, moving);
        self.tabs.active = Some(to);
        self.activate(to, window, cx);
        self.persist_session(cx);
        cx.notify();
    }

    /// ⌘\: open the active file in the other group too (a new group on the right if unsplit); focus
    /// goes there. One document, two views: typing in one shows in the other.
    pub(super) fn split_editor(&mut self, _: &SplitEditor, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.diff_active = false;
        let Some(ed) = self.tabs.active.and_then(|a| self.tabs.editors.get(a)).cloned() else { return };
        if self.tabs.other.is_none() {
            self.tabs.focused_left = true; // the new group opens on the right
        }
        let g = self.tabs.other.get_or_insert(Group { editors: Vec::new(), active: None });
        let ix = match g.editors.iter().position(|e| *e == ed) {
            Some(i) => i,
            None => {
                g.editors.push(ed);
                g.editors.len() - 1
            }
        };
        g.active = Some(ix);
        self.swap_groups();
        self.activate(ix, window, cx);
        self.persist_session(cx);
        cx.notify();
    }

    /// Move the active tab to the other group (a new group on the right if unsplit; with a single tab
    /// the right group opens empty). If it's already open in the other group, switch to that one.
    pub(super) fn move_editor_to_other_group(&mut self, _: &MoveEditorToOtherGroup, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.diff_active = false;
        let split = self.tabs.other.is_some();
        if !split && self.tabs.editors.is_empty() {
            return; // nothing to split
        }
        // Tab to move: an unsplit lone tab isn't moved (else the left group would end up empty).
        let moving = match self.tabs.active {
            Some(ix) if split || self.tabs.editors.len() > 1 => {
                let ed = self.tabs.editors.remove(ix);
                self.tabs.active = (!self.tabs.editors.is_empty()).then(|| ix.min(self.tabs.editors.len() - 1));
                Some(ed)
            }
            _ => None,
        };
        // The tab in front of the group left behind stays active (syntax tree).
        if let Some(a) = self.tabs.active {
            self.activate(a, window, cx);
        }
        let g = self.tabs.other.get_or_insert(Group { editors: Vec::new(), active: None });
        if let Some(ed) = moving {
            let ix = g.editors.iter().position(|e| *e == ed).unwrap_or_else(|| {
                g.editors.push(ed);
                g.editors.len() - 1
            });
            g.active = Some(ix);
        }
        if !split {
            self.tabs.focused_left = true; // the new group opens on the right
        }
        self.swap_groups(); // focus goes with the moved tab (or the empty new group)
        // The group whose last tab was moved closes.
        if !self.tabs.editors.is_empty() && self.tabs.other.as_ref().is_some_and(|g| g.editors.is_empty()) {
            self.tabs.other = None;
            self.tabs.focused_left = true;
        }
        match self.tabs.active {
            Some(a) => self.activate(a, window, cx),
            None => window.focus(&self.focus_handle),
        }
        self.persist_session(cx);
        cx.notify();
    }
}
