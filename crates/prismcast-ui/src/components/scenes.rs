//! The scenes panel: the ordered scene list with selection and an add
//! button, per PLAN.md §28.

use crate::bridge::SnapshotRefresh;
use adw::prelude::*;
use prismcast_app::AppSnapshot;
use prismcast_core::id::SceneId;
use relm4::{ComponentParts, ComponentSender, SimpleComponent};

/// The scenes panel. Rebuilds its list from each snapshot refresh.
pub struct ScenesPanel {
    list: gtk::ListBox,
    /// Scene IDs in displayed row order; row index ↔ `order[index]`.
    order: Vec<SceneId>,
    /// The current (program) scene, for the highlight and the re-dispatch
    /// guard.
    current: Option<SceneId>,
    /// True while programmatically rebuilding/selecting rows, so the
    /// selection handler does not echo commands back for UI-driven changes.
    restoring: bool,
}

#[derive(Debug)]
pub enum ScenesInput {
    /// A fresh snapshot; the list is rebuilt from it.
    Refresh(SnapshotRefresh),
    /// A row was selected by the user (row index).
    RowSelected(usize),
}

#[derive(Debug)]
pub enum ScenesOutput {
    /// The user picked a scene; the root should dispatch `SetCurrentScene`.
    Select(SceneId),
    /// The user clicked the add button; the root should ask for a name.
    AddRequested,
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
        {
            let input = sender.input_sender().clone();
            list.connect_row_selected(move |_, row| {
                if let Some(row) = row {
                    let index = usize::try_from(row.index()).unwrap_or(0);
                    input.emit(ScenesInput::RowSelected(index));
                }
            });
        }

        let model = Self {
            list,
            order: Vec::new(),
            current: None,
            restoring: false,
        };
        ComponentParts { model, widgets: () }
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>) {
        match message {
            ScenesInput::RowSelected(index) => {
                if self.restoring {
                    return;
                }
                if let Some(&scene_id) = self.order.get(index) {
                    if self.current != Some(scene_id) {
                        sender.output_sender().emit(ScenesOutput::Select(scene_id));
                    }
                }
            }
            ScenesInput::Refresh(snapshot) => self.refresh(&snapshot.read()),
        }
    }
}

impl ScenesPanel {
    /// Rebuilds the list from the snapshot, highlighting and selecting the
    /// current (program) scene.
    fn refresh(&mut self, snapshot: &AppSnapshot) {
        self.restoring = true;
        self.list.unselect_all();
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        self.order.clear();

        let current = snapshot.current_scene();
        let mut current_row = None;
        for scene in snapshot.scenes() {
            let label = gtk::Label::new(Some(&scene.name));
            label.set_xalign(0.0);
            label.set_margin_start(6);
            if current == Some(scene.id) {
                label.add_css_class("heading");
            }
            let row = gtk::ListBoxRow::new();
            row.set_child(Some(&label));
            self.list.append(&row);
            if current == Some(scene.id) {
                current_row = Some(row);
            }
            self.order.push(scene.id);
        }
        if let Some(row) = current_row {
            self.list.select_row(Some(&row));
        }
        self.current = current;
        self.restoring = false;
    }
}
