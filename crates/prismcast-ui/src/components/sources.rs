//! The sources panel: the shared source list with an add button, per
//! PLAN.md §28.

use std::sync::Arc;

use adw::prelude::*;
use prismcast_app::AppSnapshot;
use relm4::{ComponentParts, ComponentSender, SimpleComponent};

use crate::presentation::source_kind_label;

/// The sources panel. Rebuilds its list from each snapshot refresh.
pub struct SourcesPanel {
    list: gtk::ListBox,
}

#[derive(Debug)]
pub enum SourcesInput {
    /// A fresh snapshot; the list is rebuilt from it.
    Refresh(Arc<AppSnapshot>),
}

#[derive(Debug)]
pub enum SourcesOutput {
    /// The user clicked the add button; the root should ask for kind/name.
    AddRequested,
}

impl SimpleComponent for SourcesPanel {
    type Input = SourcesInput;
    type Output = SourcesOutput;
    type Init = ();
    type Root = gtk::Frame;
    type Widgets = ();

    fn init_root() -> Self::Root {
        gtk::Frame::new(Some("Sources"))
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
        list.set_selection_mode(gtk::SelectionMode::None);
        scrolled.set_child(Some(&list));
        container.append(&scrolled);

        let add_button = gtk::Button::with_label("+");
        add_button.set_tooltip_text(Some("Add source"));
        container.append(&add_button);
        root.set_child(Some(&container));

        add_button.connect_clicked(move |_| {
            sender.output_sender().emit(SourcesOutput::AddRequested);
        });

        let model = Self { list };
        ComponentParts { model, widgets: () }
    }

    fn update(&mut self, message: Self::Input, _sender: ComponentSender<Self>) {
        match message {
            SourcesInput::Refresh(snapshot) => self.refresh(&snapshot),
        }
    }
}

impl SourcesPanel {
    /// Rebuilds the list from the snapshot: one row per source, showing name
    /// and kind.
    fn refresh(&mut self, snapshot: &AppSnapshot) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        for source in snapshot.sources() {
            let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            row_box.set_margin_start(6);
            row_box.set_margin_end(6);

            let name = gtk::Label::new(Some(&source.name));
            name.set_xalign(0.0);
            name.set_hexpand(true);
            row_box.append(&name);

            let kind = gtk::Label::new(Some(source_kind_label(&source.kind)));
            kind.add_css_class("dim-label");
            kind.add_css_class("caption");
            row_box.append(&kind);

            let row = gtk::ListBoxRow::new();
            row.set_child(Some(&row_box));
            self.list.append(&row);
        }
    }
}
