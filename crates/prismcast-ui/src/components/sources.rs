//! The sources panel: the shared source list with an add button, per
//! PLAN.md §28.

use crate::bridge::SnapshotRefresh;
use prismcast_core::{Command, SourceId};

use adw::prelude::*;
use prismcast_app::AppSnapshot;
use relm4::{ComponentParts, ComponentSender, SimpleComponent};

use crate::presentation::source_kind_label;

/// The sources panel. Rebuilds its list from each snapshot refresh.
pub struct SourcesPanel {
    list: gtk::ListBox,
    empty: gtk::Label,
}

#[derive(Debug)]
pub enum SourcesInput {
    /// A fresh snapshot; the list is rebuilt from it.
    Refresh(SnapshotRefresh),
    Command(Box<Command>),
    Rename {
        source_id: SourceId,
        name: String,
    },
}

#[derive(Debug)]
pub enum SourcesOutput {
    /// The user clicked the add button; the root should ask for kind/name.
    AddRequested,
    Command(Box<Command>),
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
        let empty = gtk::Label::new(Some("Select or add a scene to place sources."));
        empty.set_wrap(true);
        empty.add_css_class("dim-label");
        container.append(&empty);

        let add_button = gtk::Button::with_label("+");
        add_button.set_tooltip_text(Some("Add source"));
        container.append(&add_button);
        root.set_child(Some(&container));

        add_button.connect_clicked(move |_| {
            sender.output_sender().emit(SourcesOutput::AddRequested);
        });

        let model = Self { list, empty };
        ComponentParts { model, widgets: () }
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>) {
        match message {
            SourcesInput::Refresh(refresh) => self.refresh(&refresh.read(), &sender),
            SourcesInput::Command(command) => {
                sender.output_sender().emit(SourcesOutput::Command(command));
            }
            SourcesInput::Rename { source_id, name } => {
                let dialog = adw::Dialog::new();
                dialog.set_title("Rename Shared Source");
                dialog.set_content_width(360);
                let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
                body.set_margin_top(12);
                body.set_margin_bottom(12);
                body.set_margin_start(12);
                body.set_margin_end(12);
                let entry = gtk::Entry::new();
                entry.set_text(&name);
                let button = gtk::Button::with_label("Rename");
                button.add_css_class("suggested-action");
                let submit = {
                    let entry = entry.clone();
                    let dialog = dialog.clone();
                    let input = sender.input_sender().clone();
                    move || {
                        let name = entry.text().trim().to_owned();
                        if !name.is_empty() {
                            input.emit(SourcesInput::Command(Box::new(Command::RenameSource {
                                source_id,
                                name,
                            })));
                            dialog.close();
                        }
                    }
                };
                button.connect_clicked({
                    let submit = submit.clone();
                    move |_| submit()
                });
                entry.connect_activate(move |_| submit());
                entry.connect_changed({
                    let button = button.clone();
                    move |entry| button.set_sensitive(!entry.text().trim().is_empty())
                });
                body.append(&entry);
                body.append(&button);
                dialog.set_child(Some(&body));
                dialog.present(Some(&self.list));
                entry.grab_focus();
            }
        }
    }
}

impl SourcesPanel {
    /// Renders scene placements separately from the shared source registry.
    fn refresh(&mut self, snapshot: &AppSnapshot, sender: &ComponentSender<Self>) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        let scene = snapshot
            .current_scene()
            .and_then(|id| snapshot.state().scenes.get(&id));
        self.empty
            .set_visible(scene.is_none_or(|scene| scene.items.is_empty()));
        self.empty.set_label(if scene.is_some() {
            "No placed sources. Add a new source or place an existing one below."
        } else {
            "Select or add a scene to place sources."
        });
        if let Some(scene) = scene {
            for item in &scene.items {
                let Some(source) = snapshot.state().sources.get(&item.source_id) else {
                    continue;
                };
                let row = source_row(&source.name, source_kind_label(&source.kind));
                let scene_id = scene.id;
                let item_id = item.id;
                for (label, active, field) in [
                    ("Visible", item.visible, ItemToggle::Visible),
                    ("Locked", item.locked, ItemToggle::Locked),
                ] {
                    let toggle = gtk::CheckButton::with_label(label);
                    // Render before connecting so snapshot initialization emits no command.
                    toggle.set_active(active);
                    let input = sender.input_sender().clone();
                    connect_item_toggle(&toggle, scene_id, item_id, field, move |command| {
                        input.emit(SourcesInput::Command(Box::new(command)));
                    });
                    row.append(&toggle);
                }
                let rename = gtk::Button::from_icon_name("document-edit-symbolic");
                rename.set_tooltip_text(Some("Rename shared source in all scenes"));
                let source_id = source.id;
                let name = source.name.clone();
                let input = sender.input_sender().clone();
                rename.connect_clicked(move |_| {
                    input.emit(SourcesInput::Rename {
                        source_id,
                        name: name.clone(),
                    })
                });
                row.append(&rename);
                command_button(
                    &row,
                    "user-trash-symbolic",
                    "Remove placement from this scene",
                    Command::RemoveSceneItem { scene_id, item_id },
                    sender,
                );
                self.list.append(&row);
            }
        }
        let header = gtk::Label::new(Some("Shared sources — place in the selected scene"));
        header.set_wrap(true);
        header.add_css_class("heading");
        self.list.append(&header);
        for source in snapshot.sources() {
            let row = source_row(&source.name, source_kind_label(&source.kind));
            if let Some(scene) = scene {
                command_button(
                    &row,
                    "list-add-symbolic",
                    "Place shared source in this scene",
                    Command::AddSceneItem {
                        scene_id: scene.id,
                        source_id: source.id,
                    },
                    sender,
                );
            }
            command_button(
                &row,
                "user-trash-symbolic",
                "Delete shared source (requires no references)",
                Command::RemoveSource {
                    source_id: source.id,
                },
                sender,
            );
            self.list.append(&row);
        }
    }
}

#[derive(Clone, Copy)]
enum ItemToggle {
    Visible,
    Locked,
}

/// Read the signal's current value: multiple user toggles can occur before the
/// next committed snapshot refresh and must not reuse its stale inverse.
fn connect_item_toggle(
    toggle: &gtk::CheckButton,
    scene_id: prismcast_core::SceneId,
    item_id: prismcast_core::SceneItemId,
    field: ItemToggle,
    send: impl Fn(Command) + 'static,
) {
    toggle.connect_toggled(move |toggle| {
        let active = toggle.is_active();
        send(match field {
            ItemToggle::Visible => Command::SetSceneItemVisible {
                scene_id,
                item_id,
                visible: active,
            },
            ItemToggle::Locked => Command::SetSceneItemLocked {
                scene_id,
                item_id,
                locked: active,
            },
        });
    });
}

fn source_row(name: &str, kind: &str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let label = gtk::Label::new(Some(name));
    label.set_xalign(0.0);
    label.set_hexpand(true);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_max_width_chars(24);
    label.set_tooltip_text(Some(name));
    row.append(&label);
    let kind = gtk::Label::new(Some(kind));
    kind.add_css_class("dim-label");
    row.append(&kind);
    row
}

fn command_button(
    row: &gtk::Box,
    icon: &str,
    tooltip: &str,
    command: Command,
    sender: &ComponentSender<SourcesPanel>,
) {
    let button = gtk::Button::from_icon_name(icon);
    button.add_css_class("flat");
    button.set_tooltip_text(Some(tooltip));
    let input = sender.input_sender().clone();
    button.connect_clicked(move |_| input.emit(SourcesInput::Command(Box::new(command.clone()))));
    row.append(&button);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};

    #[test]
    #[ignore = "requires a real GTK display; run with --ignored --test-threads=1"]
    fn rapid_visible_and_locked_toggles_keep_final_signal_value() {
        gtk::init().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let handle = prismcast_app::AppHandle::spawn(prismcast_app::CoreConfig::default());
            handle
                .dispatch(Command::AddScene {
                    name: "Toggle target".into(),
                })
                .await
                .unwrap();
            let scene_id = handle.snapshot().current_scene().unwrap();
            handle
                .dispatch(Command::AddSource {
                    kind: prismcast_core::SourceKind::TestPattern,
                    name: "Pattern".into(),
                })
                .await
                .unwrap();
            let source_id = handle.snapshot().sources().next().unwrap().id;
            handle
                .dispatch(Command::AddSceneItem {
                    scene_id,
                    source_id,
                })
                .await
                .unwrap();
            let item_id = handle.snapshot().state().scenes[&scene_id].items[0].id;
            let pending = Rc::new(RefCell::new(Vec::new()));
            for (field, initial) in [(ItemToggle::Visible, true), (ItemToggle::Locked, false)] {
                let toggle = gtk::CheckButton::new();
                toggle.set_active(initial);
                let commands = pending.clone();
                connect_item_toggle(&toggle, scene_id, item_id, field, move |command| {
                    commands.borrow_mut().push(command)
                });
                // Deliberately do not dispatch or render between the two GTK signals.
                toggle.set_active(!initial);
                toggle.set_active(initial);
            }
            let commands: Vec<_> = pending.borrow_mut().drain(..).collect();
            assert_eq!(commands.len(), 4);
            for command in commands {
                handle.dispatch(command).await.unwrap();
            }
            let snapshot = handle.snapshot();
            let item = &snapshot.state().scenes[&scene_id].items[0];
            assert!(item.visible, "second visibility signal must win");
            assert!(!item.locked, "second lock signal must win");
            handle.shutdown().await;
        });
    }
}
