//! The scenes panel: the ordered scene list with selection, rename, removal
//! (confirmation-gated), and reordering, per PLAN.md §28.
//!
//! Presentation only: every mutation leaves this component as a core
//! [`Command`] routed through the root's shared dispatcher; the list itself is
//! rebuilt from each committed snapshot (AGENTS.md central invariant).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use prismcast_app::AppSnapshot;
use prismcast_core::id::SceneId;
use prismcast_core::Command;
use relm4::{ComponentParts, ComponentSender, SimpleComponent};

use crate::bridge::SnapshotRefresh;
use crate::presentation::trimmed_name;

/// The scenes panel. Rebuilds its list from each snapshot refresh.
pub struct ScenesPanel {
    list: gtk::ListBox,
    /// Scene IDs in displayed row order; row index ↔ `order[index]`.
    order: Rc<RefCell<Vec<SceneId>>>,
    /// The current (program) scene, for the highlight and the re-dispatch
    /// guard.
    current: Option<SceneId>,
    /// True while programmatically rebuilding/selecting rows, so the
    /// selection handler does not echo commands back for UI-driven changes.
    restoring: Rc<Cell<bool>>,
    snapshot: Option<Arc<AppSnapshot>>,
}

#[derive(Debug, Clone)]
pub enum ScenesInput {
    /// A fresh snapshot notification; the list is rebuilt from it.
    Refresh(SnapshotRefresh),
    /// A row was selected by the user (row index).
    RowSelected(SceneId),
    /// Open the rename dialog for a scene.
    Rename(SceneId),
    /// The rename dialog was confirmed with a valid name.
    RenameSubmitted {
        /// Scene to rename.
        scene_id: SceneId,
        /// New, pre-validated name.
        name: String,
    },
    /// Ask whether a scene may be removed (confirmation dialog).
    Remove(SceneId),
    /// Move a scene within the list.
    Move {
        /// Scene to move.
        scene_id: SceneId,
        /// Direction (`-1` = up, `+1` = down).
        delta: isize,
    },
}

#[derive(Debug)]
pub enum ScenesOutput {
    /// The user picked a scene; the root should dispatch `SetCurrentScene`.
    Select(SceneId),
    /// The user clicked the add button; the root should ask for a name.
    AddRequested,
    /// A scene edit routed through the root core dispatcher.
    Command(Box<Command>),
}

impl SimpleComponent for ScenesPanel {
    type Input = ScenesInput;
    type Output = ScenesOutput;
    type Init = ();
    type Root = gtk::Frame;
    type Widgets = ();

    fn init_root() -> Self::Root {
        gtk::Frame::new(Some("Scenes"))
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let container = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let scrolled = gtk::ScrolledWindow::new();
        scrolled.set_vexpand(true);
        scrolled.set_propagate_natural_height(true);

        let list = gtk::ListBox::new();
        list.add_css_class("navigation-sidebar");
        list.set_selection_mode(gtk::SelectionMode::Single);
        let placeholder = adw::StatusPage::new();
        placeholder.set_icon_name(Some("view-list-symbolic"));
        placeholder.set_title("No Scenes");
        placeholder.set_description(Some("Add a scene to begin."));
        placeholder.set_vexpand(true);
        list.set_placeholder(Some(&placeholder));
        scrolled.set_child(Some(&list));
        container.append(&scrolled);

        let order = Rc::new(RefCell::new(Vec::new()));
        let restoring = Rc::new(Cell::new(false));

        let add_button = gtk::Button::with_label("+");
        add_button.set_tooltip_text(Some("Add scene"));
        container.append(&add_button);
        root.set_child(Some(&container));

        {
            let output = sender.output_sender().clone();
            add_button.connect_clicked(move |_| {
                output.emit(ScenesOutput::AddRequested);
            });
        }

        // Delete key removes the selected scene (confirmation dialog follows
        // in `update`, so a stray keypress is never destructive).
        let key_controller = gtk::EventControllerKey::new();
        key_controller.connect_key_pressed({
            let list = list.clone();
            let order = Rc::clone(&order);
            let input = sender.input_sender().clone();
            move |_, key, _, _| {
                if key != gtk::gdk::Key::Delete {
                    return gtk::glib::Propagation::Proceed;
                }
                let scene_id = list
                    .selected_row()
                    .and_then(|row| usize::try_from(row.index()).ok())
                    .and_then(|index| order.borrow().get(index).copied());
                match scene_id {
                    Some(scene_id) => {
                        input.emit(ScenesInput::Remove(scene_id));
                        gtk::glib::Propagation::Stop
                    }
                    None => gtk::glib::Propagation::Proceed,
                }
            }
        });
        list.add_controller(key_controller);

        list.connect_row_selected({
            let order = Rc::clone(&order);
            let restoring = Rc::clone(&restoring);
            let input = sender.input_sender().clone();
            move |_, row| {
                if restoring.get() {
                    return;
                }
                if let Some(scene_id) = row
                    .and_then(|row| usize::try_from(row.index()).ok())
                    .and_then(|index| order.borrow().get(index).copied())
                {
                    input.emit(ScenesInput::RowSelected(scene_id));
                }
            }
        });

        let model = Self {
            list,
            order,
            current: None,
            restoring,
            snapshot: None,
        };
        ComponentParts { model, widgets: () }
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>) {
        match message {
            ScenesInput::RowSelected(scene_id) => {
                if self.current != Some(scene_id) && self.order.borrow().contains(&scene_id) {
                    sender.output_sender().emit(ScenesOutput::Select(scene_id));
                }
            }
            ScenesInput::Refresh(refresh) => {
                let snapshot = refresh.read();
                self.refresh(&snapshot, &sender);
                self.snapshot = Some(snapshot);
            }
            ScenesInput::Rename(scene_id) => self.present_rename_dialog(scene_id, &sender),
            ScenesInput::RenameSubmitted { scene_id, name } => {
                sender.output_sender().emit(ScenesOutput::Command(Box::new(
                    Command::RenameScene { scene_id, name },
                )));
            }
            ScenesInput::Remove(scene_id) => self.present_remove_confirmation(scene_id, &sender),
            ScenesInput::Move { scene_id, delta } => {
                if let Some(new_index) = move_target(&self.order.borrow(), scene_id, delta) {
                    sender.output_sender().emit(ScenesOutput::Command(Box::new(
                        Command::ReorderScene {
                            scene_id,
                            new_index,
                        },
                    )));
                }
            }
        }
    }
}

impl ScenesPanel {
    /// Rebuilds the list from the snapshot, highlighting and selecting the
    /// current (program) scene. Selection restoration is how external
    /// controllers (CLI/WS) move the UI selection: the pump pushes the
    /// committed snapshot and this re-selects the matching row.
    fn refresh(&mut self, snapshot: &AppSnapshot, sender: &ComponentSender<Self>) {
        self.restoring.set(true);
        self.list.unselect_all();
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        self.order.borrow_mut().clear();

        let scenes: Vec<_> = snapshot.scenes().collect();
        let last_index = scenes.len().saturating_sub(1);
        let current = snapshot.current_scene();
        let mut current_row = None;
        for (index, scene) in scenes.iter().enumerate() {
            let label = gtk::Label::new(Some(&scene.name));
            label.set_xalign(0.0);
            label.set_margin_start(6);
            if current == Some(scene.id) {
                label.add_css_class("heading");
            }
            // Long names ellipsize; the full name stays available as a
            // tooltip.
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            label.set_max_width_chars(24);
            label.set_tooltip_text(Some(&scene.name));
            label.set_hexpand(true);

            let body = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            body.append(&label);
            let scene_id = scene.id;
            for (icon, tooltip, message, sensitive) in [
                (
                    "document-edit-symbolic",
                    "Rename scene",
                    ScenesInput::Rename(scene_id),
                    true,
                ),
                (
                    "user-trash-symbolic",
                    "Remove scene and its items",
                    ScenesInput::Remove(scene_id),
                    true,
                ),
                (
                    "go-up-symbolic",
                    "Move scene up",
                    ScenesInput::Move {
                        scene_id,
                        delta: -1,
                    },
                    index > 0,
                ),
                (
                    "go-down-symbolic",
                    "Move scene down",
                    ScenesInput::Move { scene_id, delta: 1 },
                    index < last_index,
                ),
            ] {
                let button = gtk::Button::from_icon_name(icon);
                button.add_css_class("flat");
                button.set_tooltip_text(Some(tooltip));
                button.set_sensitive(sensitive);
                let input = sender.input_sender().clone();
                button.connect_clicked(move |_| input.emit(message.clone()));
                body.append(&button);
            }

            let row = gtk::ListBoxRow::new();
            row.set_child(Some(&body));
            self.list.append(&row);
            if current == Some(scene.id) {
                current_row = Some(row);
            }
            self.order.borrow_mut().push(scene.id);
        }
        if let Some(row) = current_row {
            self.list.select_row(Some(&row));
        }
        self.current = current;
        self.restoring.set(false);
    }

    /// Shows the rename dialog for `scene_id`. The name is pre-validated
    /// in-dialog (blank names keep "Rename" disabled, Enter confirms);
    /// duplicate-name and other core rejections surface via the root's toast.
    fn present_rename_dialog(&self, scene_id: SceneId, sender: &ComponentSender<Self>) {
        let Some(scene) = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.state().scenes.get(&scene_id))
        else {
            return;
        };

        let dialog = adw::Dialog::new();
        dialog.set_title("Rename Scene");
        dialog.set_content_width(360);

        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        let rename_button = gtk::Button::with_label("Rename");
        rename_button.add_css_class("suggested-action");
        header.pack_end(&rename_button);
        toolbar.add_top_bar(&header);

        let entry = adw::EntryRow::new();
        entry.set_title("Scene name");
        entry.set_text(&scene.name);
        let group = adw::PreferencesGroup::new();
        group.add(&entry);
        let clamp = adw::Clamp::new();
        clamp.set_margin_start(12);
        clamp.set_margin_end(12);
        clamp.set_margin_top(12);
        clamp.set_margin_bottom(12);
        clamp.set_child(Some(&group));
        toolbar.set_content(Some(&clamp));
        dialog.set_child(Some(&toolbar));

        let submit = {
            let dialog = dialog.clone();
            let entry = entry.clone();
            let input = sender.input_sender().clone();
            move || {
                if let Some(name) = trimmed_name(&entry.text()) {
                    input.emit(ScenesInput::RenameSubmitted { scene_id, name });
                    dialog.close();
                }
            }
        };
        rename_button.connect_clicked({
            let submit = submit.clone();
            move |_| submit()
        });
        entry.connect_entry_activated(move |_| submit());
        entry.connect_changed({
            let rename_button = rename_button.clone();
            move |entry| rename_button.set_sensitive(trimmed_name(&entry.text()).is_some())
        });

        dialog.present(Some(&self.list));
        entry.grab_focus();
        // Preselect the current name so typing replaces it outright.
        entry.select_region(0, -1);
    }

    /// Presents a destructive-action confirmation for removing `scene_id`.
    /// Only the "remove" response dispatches `RemoveScene` — through the
    /// root's shared dispatcher, so core rejections (last scene, scene
    /// referenced by studio mode or a scene source) land in the root's toast.
    fn present_remove_confirmation(&self, scene_id: SceneId, sender: &ComponentSender<Self>) {
        let Some(scene) = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.state().scenes.get(&scene_id))
        else {
            return;
        };
        let dialog = adw::AlertDialog::new(
            Some("Remove Scene?"),
            Some(&format!(
                "\u{201c}{}\u{201d} and all its items will be removed. This cannot be undone.",
                scene.name
            )),
        );
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("remove", "Remove");
        dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let output = sender.output_sender().clone();
        dialog.connect_response(None, move |_, response| {
            if response == "remove" {
                output.emit(ScenesOutput::Command(Box::new(Command::RemoveScene {
                    scene_id,
                })));
            }
        });
        dialog.present(Some(&self.list));
    }
}

/// Computes the target index for a move, rejecting unknown IDs and moves
/// past either list boundary.
fn move_target(order: &[SceneId], scene_id: SceneId, delta: isize) -> Option<usize> {
    let index = order.iter().position(|id| *id == scene_id)?;
    let target = index.checked_add_signed(delta)?;
    (target < order.len() && target != index).then_some(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reorder_targets_respect_snapshot_order_and_boundaries() {
        let first = SceneId::new();
        let second = SceneId::new();
        let order = [first, second];
        assert_eq!(move_target(&order, first, -1), None);
        assert_eq!(move_target(&order, second, 1), None);
        assert_eq!(move_target(&order, first, 1), Some(1));
        assert_eq!(move_target(&order, second, -1), Some(0));
        assert_eq!(move_target(&order, SceneId::new(), 1), None);
    }
}
