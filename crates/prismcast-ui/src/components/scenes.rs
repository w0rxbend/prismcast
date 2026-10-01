//! The scenes panel: the ordered scene list with selection and an add
//! button, per PLAN.md §28.

use prismcast_core::Command;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use crate::bridge::SnapshotRefresh;
use adw::prelude::*;
use prismcast_app::AppSnapshot;
use prismcast_core::id::SceneId;
use relm4::{ComponentParts, ComponentSender, SimpleComponent};

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
    empty: gtk::Label,
    snapshot: Option<Arc<AppSnapshot>>,
}

#[derive(Debug, Clone)]
pub enum ScenesInput {
    /// A fresh snapshot; the list is rebuilt from it.
    Refresh(SnapshotRefresh),
    /// A row was selected by the user (row index).
    RowSelected(SceneId),
    Rename(SceneId),
    RenameSubmitted {
        scene_id: SceneId,
        name: String,
    },
    Remove(SceneId),
    Move {
        scene_id: SceneId,
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
        scrolled.set_child(Some(&list));
        container.append(&scrolled);
        let empty = gtk::Label::new(Some("No scenes. Add a scene to begin."));
        empty.set_wrap(true);
        empty.add_css_class("dim-label");
        container.append(&empty);
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

        let model = Self {
            list,
            order: Rc::new(RefCell::new(Vec::new())),
            current: None,
            restoring,
            empty,
            snapshot: None,
        };
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed({
            let list = model.list.clone();
            let order = model.order.clone();
            let input = sender.input_sender().clone();
            move |_, key, _, _| {
                if key != gtk::gdk::Key::Delete {
                    return gtk::glib::Propagation::Proceed;
                }
                if let Some(scene_id) = list
                    .selected_row()
                    .and_then(|row| usize::try_from(row.index()).ok())
                    .and_then(|index| order.borrow().get(index).copied())
                {
                    input.emit(ScenesInput::Remove(scene_id));
                    gtk::glib::Propagation::Stop
                } else {
                    gtk::glib::Propagation::Proceed
                }
            }
        });
        model.list.add_controller(keys);
        model.list.connect_row_selected({
            let order = model.order.clone();
            let restoring = model.restoring.clone();
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
            ScenesInput::Rename(scene_id) => {
                let Some(scene) = self
                    .snapshot
                    .as_ref()
                    .and_then(|s| s.state().scenes.get(&scene_id))
                else {
                    return;
                };
                let input = sender.input_sender().clone();
                let (dialog, entry, _) = rename_dialog(&scene.name, move |name| {
                    input.emit(ScenesInput::RenameSubmitted { scene_id, name });
                });
                present_rename(&dialog, &entry, &self.list);
            }
            ScenesInput::RenameSubmitted { scene_id, name } => {
                sender.output_sender().emit(ScenesOutput::Command(Box::new(
                    Command::RenameScene { scene_id, name },
                )));
            }
            ScenesInput::Remove(scene_id) => {
                let Some(scene) = self
                    .snapshot
                    .as_ref()
                    .and_then(|snapshot| snapshot.state().scenes.get(&scene_id))
                else {
                    return;
                };
                let output = sender.output_sender().clone();
                let dialog = remove_dialog(&scene.name, move || {
                    output.emit(ScenesOutput::Command(Box::new(Command::RemoveScene {
                        scene_id,
                    })));
                });
                dialog.present(Some(&self.list));
            }

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
    /// current (program) scene.
    fn refresh(&mut self, snapshot: &AppSnapshot, sender: &ComponentSender<Self>) {
        self.restoring.set(true);
        self.list.unselect_all();
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        self.order.borrow_mut().clear();

        self.empty.set_visible(snapshot.scenes().next().is_none());
        let current = snapshot.current_scene();
        let mut current_row = None;
        let scene_count = snapshot.scenes().count();
        for (index, scene) in snapshot.scenes().enumerate() {
            let label = gtk::Label::new(Some(&scene.name));
            label.set_xalign(0.0);
            label.set_margin_start(6);
            if current == Some(scene.id) {
                label.add_css_class("heading");
            }
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            label.set_max_width_chars(24);
            label.set_tooltip_text(Some(&scene.name));
            label.set_hexpand(true);
            let row = gtk::ListBoxRow::new();
            let body = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            body.append(&label);
            let scene_id = scene.id;
            for (title, message) in [
                ("Rename", ScenesInput::Rename(scene_id)),
                ("Remove", ScenesInput::Remove(scene_id)),
                (
                    "↑",
                    ScenesInput::Move {
                        scene_id,
                        delta: -1,
                    },
                ),
                ("↓", ScenesInput::Move { scene_id, delta: 1 }),
            ] {
                let button = gtk::Button::from_icon_name(match title {
                    "↑" => "go-up-symbolic",
                    "↓" => "go-down-symbolic",
                    "Remove" => "user-trash-symbolic",
                    _ => "document-edit-symbolic",
                });
                button.add_css_class("flat");
                button.set_sensitive(match title {
                    "↑" => index > 0,
                    "↓" => index + 1 < scene_count,
                    _ => true,
                });
                button.set_tooltip_text(Some(match title {
                    "↑" => "Move scene up",
                    "↓" => "Move scene down",
                    "Remove" => "Remove scene and its items",
                    _ => "Rename scene",
                }));
                let input = sender.input_sender().clone();
                button.connect_clicked(move |_| input.emit(message.clone()));
                body.append(&button);
            }
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
}

/// Labeled editable row and shared validation for button/Enter submission.
fn rename_dialog(
    name: &str,
    send: impl Fn(String) + 'static,
) -> (adw::Dialog, adw::EntryRow, gtk::Button) {
    let dialog = adw::Dialog::new();
    dialog.set_title("Rename Scene");
    dialog.set_content_width(360);
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    let button = gtk::Button::with_label("Rename");
    button.add_css_class("suggested-action");
    header.pack_end(&button);
    toolbar.add_top_bar(&header);
    let entry = adw::EntryRow::new();
    entry.set_title("Scene name");
    entry.set_text(name);
    let group = adw::PreferencesGroup::new();
    group.add(&entry);
    let clamp = adw::Clamp::new();
    clamp.set_margin_top(12);
    clamp.set_margin_bottom(12);
    clamp.set_margin_start(12);
    clamp.set_margin_end(12);
    clamp.set_child(Some(&group));
    toolbar.set_content(Some(&clamp));
    dialog.set_child(Some(&toolbar));
    let submit = {
        // Signals live on children of the dialog; weak captures avoid keeping
        // the closed dialog/editor alive through their own signal closures.
        let entry = entry.downgrade();
        let dialog = dialog.downgrade();
        Rc::new(move || {
            let Some(entry) = entry.upgrade() else {
                return;
            };
            let name = entry.text().trim().to_owned();
            if !name.is_empty() {
                send(name);
                if let Some(dialog) = dialog.upgrade() {
                    dialog.close();
                }
            }
        })
    };
    button.connect_clicked({
        let submit = submit.clone();
        move |_| submit()
    });
    entry.connect_entry_activated(move |_| submit());
    button.set_sensitive(!entry.text().trim().is_empty());
    entry.connect_changed({
        let button = button.clone();
        move |entry| button.set_sensitive(!entry.text().trim().is_empty())
    });
    (dialog, entry, button)
}

fn present_rename(dialog: &adw::Dialog, entry: &adw::EntryRow, parent: &impl IsA<gtk::Widget>) {
    dialog.present(Some(parent));
    entry.grab_focus();
    entry.select_region(0, -1);
}

/// Removal has no undo inverse in the current core; cancel is the safe default.
fn remove_dialog(name: &str, send: impl Fn() + 'static) -> adw::AlertDialog {
    let dialog = adw::AlertDialog::new(
        Some("Remove Scene?"),
        Some(&format!(
            "“{name}” and all its items will be removed. This cannot be undone."
        )),
    );
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("remove", "Remove");
    dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.connect_response(None, move |_, response| {
        if response == "remove" {
            send();
        }
    });
    dialog
}

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

    #[test]
    #[ignore = "requires a real GTK display; run separately with --ignored --test-threads=1"]
    fn scene_dialog_signals_validate_names_and_require_explicit_removal() {
        adw::init().unwrap();
        let window = adw::Window::new();
        window.set_default_size(400, 300);
        window.present();
        let names = Rc::new(RefCell::new(Vec::new()));
        let captured = names.clone();
        let (dialog, entry, rename) = rename_dialog("Current scene", move |name| {
            captured.borrow_mut().push(name)
        });
        present_rename(&dialog, &entry, &window);
        let context = gtk::glib::MainContext::default();
        for _ in 0..100 {
            if !context.pending() {
                break;
            }
            context.iteration(false);
        }
        assert_eq!(entry.title(), "Scene name");
        assert_eq!(entry.selection_bounds(), Some((0, 13)));
        let focus = gtk::prelude::GtkWindowExt::focus(&window).expect("rename entry focus");
        assert!(
            focus.is_ancestor(&entry),
            "focus must be within the labeled editable row"
        );
        entry.set_text("  ");
        assert!(!rename.is_sensitive());
        entry.emit_by_name::<()>("entry-activated", &[]);
        assert!(names.borrow().is_empty());
        entry.set_text("  Renamed scene  ");
        assert!(rename.is_sensitive());
        entry.emit_by_name::<()>("entry-activated", &[]);
        assert_eq!(*names.borrow(), vec!["Renamed scene"]);
        let removals = Rc::new(Cell::new(0));
        let captured = removals.clone();
        let confirmation = remove_dialog("Current scene", move || captured.set(captured.get() + 1));
        assert_eq!(confirmation.default_response().as_deref(), Some("cancel"));
        assert_eq!(confirmation.close_response(), "cancel");
        confirmation.emit_by_name::<()>("response", &[&"cancel"]);
        assert_eq!(removals.get(), 0);
        confirmation.emit_by_name::<()>("response", &[&"remove"]);
        assert_eq!(removals.get(), 1);
        window.close();
    }
}
