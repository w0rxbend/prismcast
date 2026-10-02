//! Read-only target discovery and GTK-local audio source selection (ADR-0024).
use adw::prelude::*;
use prismcast_capture::audio::{discover_audio_targets, AudioTargetInfo};
use prismcast_core::{PipeWireAudioMode, PipeWireAudioSettings, SourceKind};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

#[derive(Debug, Clone)]
pub struct AudioSelection {
    pub name: String,
    pub kind: SourceKind,
    pub settings: PipeWireAudioSettings,
}
struct Picker {
    dialog: adw::Dialog,
    mode: gtk::DropDown,
    target: gtk::DropDown,
    name: adw::EntryRow,
    add: gtk::Button,
    status: gtk::Label,
    refresh: gtk::Button,
    pending: Cell<bool>,
    targets: Rc<RefCell<Vec<AudioTargetInfo>>>,
    visible: Rc<RefCell<Vec<AudioTargetInfo>>>,
}
fn mode(index: u32) -> PipeWireAudioMode {
    match index {
        1 => PipeWireAudioMode::Output,
        2 => PipeWireAudioMode::Application,
        _ => PipeWireAudioMode::Input,
    }
}
impl Picker {
    fn new(send: impl Fn(AudioSelection) + 'static) -> Self {
        let dialog = adw::Dialog::new();
        dialog.set_title("Add audio capture");
        dialog.set_content_width(420);
        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        let add = gtk::Button::with_label("Add");
        add.add_css_class("suggested-action");
        add.set_sensitive(false);
        header.pack_end(&add);
        toolbar.add_top_bar(&header);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.set_margin_start(12);
        content.set_margin_end(12);
        content.set_margin_top(12);
        content.set_margin_bottom(12);
        let name = adw::EntryRow::new();
        name.set_title("Source name");
        name.set_text("Audio capture");
        let group = adw::PreferencesGroup::new();
        group.add(&name);
        content.append(&group);
        let mode = gtk::DropDown::from_strings(&[
            "Microphone input",
            "System audio (sink monitor)",
            "Application playback stream",
        ]);
        mode.set_tooltip_text(Some("Audio capture type"));
        content.append(&mode);
        let target = gtk::DropDown::from_strings(&["Discovering audio targets…"]);
        target.set_sensitive(false);
        content.append(&target);
        let status = gtk::Label::new(Some("Discovering audio targets…"));
        status.set_wrap(true);
        status.add_css_class("dim-label");
        content.append(&status);
        let note = gtk::Label::new(Some("Adding saves the selection. Use Start capture in the mixer to begin. Application capture selects one current playback stream."));
        note.set_wrap(true);
        note.add_css_class("caption");
        content.append(&note);
        let refresh = gtk::Button::with_label("Refresh targets");
        content.append(&refresh);
        toolbar.set_content(Some(&content));
        dialog.set_child(Some(&toolbar));
        let targets = Rc::new(RefCell::new(Vec::new()));
        let visible = Rc::new(RefCell::new(Vec::new()));
        mode.connect_selected_notify({
            let targets = targets.clone();
            let visible = visible.clone();
            let target = target.downgrade();
            let add = add.downgrade();
            let name = name.downgrade();
            let status = status.downgrade();
            move |choice| {
                if let (Some(target), Some(add), Some(name), Some(status)) = (
                    target.upgrade(),
                    add.upgrade(),
                    name.upgrade(),
                    status.upgrade(),
                ) {
                    refresh_targets(
                        choice.selected(),
                        &targets.borrow(),
                        &visible,
                        &target,
                        &add,
                        &name,
                        &status,
                    );
                }
            }
        });
        let validate = {
            let visible = visible.clone();
            let target = target.downgrade();
            let add = add.downgrade();
            let name = name.downgrade();
            Rc::new(move || {
                if let (Some(target), Some(add), Some(name)) =
                    (target.upgrade(), add.upgrade(), name.upgrade())
                {
                    add.set_sensitive(
                        !name.text().trim().is_empty()
                            && visible.borrow().get(target.selected() as usize).is_some(),
                    );
                }
            })
        };
        target.connect_selected_notify({
            let validate = validate.clone();
            move |_| validate()
        });
        name.connect_changed(move |_| validate());
        add.connect_clicked({
            let visible = visible.clone();
            let target = target.downgrade();
            let name = name.downgrade();
            let dialog = dialog.downgrade();
            move |_| {
                if let (Some(target), Some(name), Some(dialog)) =
                    (target.upgrade(), name.upgrade(), dialog.upgrade())
                {
                    let name = name.text().trim().to_owned();
                    if !name.is_empty() {
                        if let Some(target) = visible.borrow().get(target.selected() as usize) {
                            let kind = if target.mode == PipeWireAudioMode::Application {
                                SourceKind::PipeWireAppAudio
                            } else {
                                SourceKind::PipeWireAudioInput
                            };
                            send(AudioSelection {
                                name,
                                kind,
                                settings: PipeWireAudioSettings {
                                    schema_version: 1,
                                    target: target.node_name.clone(),
                                    mode: target.mode,
                                },
                            });
                            dialog.close();
                        }
                    }
                }
            }
        });
        Self {
            dialog,
            mode,
            target,
            name,
            add,
            status,
            refresh,
            pending: Cell::new(false),
            targets,
            visible,
        }
    }
    fn loaded(&self, result: Result<Vec<AudioTargetInfo>, String>) {
        self.pending.set(false);
        self.refresh.set_sensitive(true);
        match result {
            Ok(targets) => {
                *self.targets.borrow_mut() = targets;
                refresh_targets(
                    self.mode.selected(),
                    &self.targets.borrow(),
                    &self.visible,
                    &self.target,
                    &self.add,
                    &self.name,
                    &self.status,
                );
            }
            Err(message) => {
                self.status
                    .set_label(&format!("Audio discovery unavailable: {message}"));
                self.add.set_sensitive(false);
                self.target.set_sensitive(false);
            }
        }
    }
}
fn refresh_targets(
    index: u32,
    targets: &[AudioTargetInfo],
    visible: &RefCell<Vec<AudioTargetInfo>>,
    target: &gtk::DropDown,
    add: &gtk::Button,
    name: &adw::EntryRow,
    status: &gtk::Label,
) {
    *visible.borrow_mut() = targets
        .iter()
        .filter(|target| target.mode == mode(index))
        .cloned()
        .collect();
    let labels: Vec<String> = visible
        .borrow()
        .iter()
        .map(|target| format!("{} ({})", target.label, target.node_name))
        .collect();
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    let model = gtk::StringList::new(if labels.is_empty() {
        &["No matching audio targets"]
    } else {
        &labels
    });
    target.set_model(Some(&model));
    target.set_selected(0);
    target.set_sensitive(!labels.is_empty());
    add.set_sensitive(!labels.is_empty() && !name.text().trim().is_empty());
    status.set_label(if labels.is_empty() {
        "No matching targets. Application streams appear while playing. Refresh to discover again."
    } else {
        "Select a target, then add the source."
    });
}
/// Discovery never opens a stream. Native enumeration runs outside GTK/Tokio.
pub fn present(parent: &impl IsA<gtk::Widget>, send: impl Fn(AudioSelection) + 'static) {
    let picker = Rc::new(Picker::new(send));
    picker.refresh.connect_clicked({
        let picker = Rc::downgrade(&picker);
        move |_| {
            if let Some(picker) = picker.upgrade() {
                discover(picker);
            }
        }
    });
    picker.dialog.present(Some(parent));
    discover(picker.clone());
    // Keep only the presentation wrapper while open; discovery closures carry
    // weak refs so closing during a probe never touches a destroyed dialog.
    let lifetime = Rc::new(RefCell::new(Some(picker.clone())));
    picker.dialog.connect_closed(move |_| {
        lifetime.borrow_mut().take();
    });
}
fn discover(picker: Rc<Picker>) {
    if picker.pending.replace(true) {
        return;
    }
    picker.refresh.set_sensitive(false);
    picker.targets.borrow_mut().clear();
    picker.visible.borrow_mut().clear();
    picker.add.set_sensitive(false);
    picker.target.set_sensitive(false);
    picker.status.set_label("Discovering audio targets…");
    let (tx, rx) = tokio::sync::oneshot::channel();
    let launched = std::thread::Builder::new()
        .name("prismcast-audio-targets".into())
        .spawn(move || {
            let _ = tx.send(discover_audio_targets().map_err(|error| error.to_string()));
        });
    if let Err(error) = launched {
        picker.loaded(Err(error.to_string()));
        return;
    }
    let weak = Rc::downgrade(&picker);
    gtk::glib::MainContext::default().spawn_local(async move {
        if let Ok(result) = rx.await {
            if let Some(picker) = weak.upgrade() {
                picker.loaded(result);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires a real GTK display; run separately with --ignored --test-threads=1"]
    fn audio_picker_handles_missing_targets_and_submits_selected_mode_without_starting_capture() {
        adw::init().unwrap();
        let selections = Rc::new(RefCell::new(Vec::new()));
        let captured = selections.clone();
        let picker = Picker::new(move |selection| captured.borrow_mut().push(selection));
        picker.loaded(Ok(Vec::new()));
        assert!(!picker.add.is_sensitive());
        picker.add.emit_clicked();
        assert!(selections.borrow().is_empty());
        picker.loaded(Err("fixture unavailable".into()));
        assert!(picker.status.text().contains("fixture unavailable"));
        picker.loaded(Ok(vec![
            AudioTargetInfo {
                node_name: "fixture.mic".into(),
                label: "Fixture microphone".into(),
                mode: PipeWireAudioMode::Input,
            },
            AudioTargetInfo {
                node_name: "fixture.sink".into(),
                label: "Fixture output".into(),
                mode: PipeWireAudioMode::Output,
            },
            AudioTargetInfo {
                node_name: "fixture.app".into(),
                label: "Fixture application".into(),
                mode: PipeWireAudioMode::Application,
            },
        ]));
        picker.name.set_text("   ");
        assert!(!picker.add.is_sensitive());
        picker.name.set_text(" Selected audio ");
        picker.mode.set_selected(2);
        assert!(picker.add.is_sensitive());
        assert_eq!(picker.visible.borrow()[0].node_name, "fixture.app");
        let window = adw::Window::new();
        window.present();
        picker.dialog.present(Some(&window));
        picker.add.emit_clicked();
        assert_eq!(selections.borrow().len(), 1);
        let selection = selections.borrow()[0].clone();
        assert_eq!(selection.name, "Selected audio");
        assert_eq!(selection.kind, SourceKind::PipeWireAppAudio);
        assert_eq!(selection.settings.target, "fixture.app");
        assert_eq!(selection.settings.mode, PipeWireAudioMode::Application);
        selection
            .settings
            .validate_for_kind(selection.kind)
            .unwrap();
        window.close();
        // Choosing a source returns settings only; no owner or authorization API
        // is reachable from this picker callback.
    }
}
