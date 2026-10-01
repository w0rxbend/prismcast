//! The root Relm4 component: main window layout per PLAN.md §28 and the only
//! place that talks to the core.
//!
//! Every user action becomes a [`Command`] dispatched through
//! [`AppHandle`] (as a Relm4 *command* whose result lands in
//! [`AppModel::update_cmd`]); every UI update flows from the core's event
//! stream — pumped in as [`AppMsg::Pump`] by [`CoreBridge`] — which triggers a
//! fresh [`AppSnapshot`] read pushed down to the panels. The UI never mutates
//! domain state directly (AGENTS.md central invariant, PLAN.md §76).

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use prismcast_app::{AppSnapshot, CommandResponse, HandleError};
use prismcast_core::id::{EncoderId, OutputId, SceneId};
use prismcast_core::output::{Output, OutputKind};
use prismcast_core::source::SourceKind;
use prismcast_core::Command;
use relm4::component::{AsyncComponent, AsyncComponentParts};
use relm4::{AsyncComponentSender, Component, ComponentController, Controller};
use tracing::{debug, info, warn};

use crate::bridge::{CoreBridge, PumpEvent};
use crate::components::outputs::{OutputsInput, OutputsOutput, OutputsPanel};
use crate::components::scenes::{ScenesInput, ScenesOutput, ScenesPanel};
use crate::components::sources::{SourcesInput, SourcesOutput, SourcesPanel};
use crate::presentation::{choice_for_transition, transition_for_choice, StreamStatus};

/// Kinds offered by the add-source dialog, in picker order.
const SOURCE_KIND_CHOICES: [(SourceKind, &str); 2] = [
    (SourceKind::TestPattern, "Test Pattern"),
    (SourceKind::Color, "Solid Color"),
];

/// Root component inputs: user intents plus pumped core events.
#[derive(Debug)]
pub enum AppMsg {
    /// An item from the core event stream (sent cross-thread by the pump).
    Pump(PumpEvent),
    /// The user selected a scene in the scenes panel.
    SelectScene(SceneId),
    /// The user clicked "+" in the scenes panel.
    AddSceneRequested,
    /// The add-scene dialog was confirmed.
    AddSceneSubmitted(String),
    /// The user clicked "+" in the sources panel.
    AddSourceRequested,
    /// The add-source dialog was confirmed.
    AddSourceSubmitted {
        /// Chosen source kind.
        kind: SourceKind,
        /// Chosen source name.
        name: String,
    },
    /// The user clicked "+" in the outputs panel.
    AddOutputRequested,
    /// The user clicked an output's start button.
    StartOutput(OutputId),
    /// The user clicked an output's stop button.
    StopOutput(OutputId),
    /// The transition selector changed (dropdown index).
    TransitionSelected(u32),
    /// The core actor finished shutting down; the window may close now.
    FinishShutdown,
}

/// Results of commands dispatched to the core actor.
#[derive(Debug)]
pub enum AppCmd {
    /// A command dispatch returned (success or rejection).
    Dispatched(Result<CommandResponse, HandleError>),
}

/// The root application model. Holds the core bridge, the panel controllers,
/// and the widgets it updates directly — presentation state only.
pub struct AppModel {
    bridge: CoreBridge,
    scenes: Controller<ScenesPanel>,
    sources: Controller<SourcesPanel>,
    outputs: Controller<OutputsPanel>,
    profile_label: gtk::Label,
    collection_label: gtk::Label,
    status_label: gtk::Label,
    transition_dropdown: gtk::DropDown,
    toast_overlay: adw::ToastOverlay,
    /// True while the dropdown is being synced from a snapshot, so the
    /// `selected` handler does not echo a `SetTransition` command back.
    syncing_transition: Cell<bool>,
}

impl AppModel {
    /// Dispatches a command to the core actor as a Relm4 command; the reply
    /// arrives as [`AppCmd::Dispatched`] in `update_cmd`.
    fn dispatch(&self, sender: &AsyncComponentSender<Self>, command: Command) {
        debug!(command = command.label(), "dispatching command");
        let handle = self.bridge.handle().clone();
        sender.oneshot_command(async move { AppCmd::Dispatched(handle.dispatch(command).await) });
    }

    /// Pushes the latest snapshot into the panels and refreshes the header
    /// and transition selector.
    fn publish_snapshot(&self) {
        let snapshot = self.bridge.handle().snapshot();
        self.scenes
            .emit(ScenesInput::Refresh(Arc::clone(&snapshot)));
        self.sources
            .emit(SourcesInput::Refresh(Arc::clone(&snapshot)));
        self.outputs
            .emit(OutputsInput::Refresh(Arc::clone(&snapshot)));
        self.sync_header(&snapshot);
        self.sync_transition(&snapshot);
    }

    /// Updates the header bar labels from the snapshot: active profile,
    /// active scene collection, and aggregate stream status.
    fn sync_header(&self, snapshot: &AppSnapshot) {
        let state = snapshot.state();
        let profile = state
            .active_profile
            .and_then(|id| state.profiles.get(&id))
            .map(|profile| profile.name.as_str());
        self.profile_label
            .set_label(&format!("Profile: {}", profile.unwrap_or("—")));
        let collection = state
            .active_collection
            .and_then(|id| state.collections.get(&id))
            .map(|collection| collection.name.as_str());
        self.collection_label
            .set_label(&format!("Collection: {}", collection.unwrap_or("—")));

        let status = StreamStatus::from_states(state.outputs.values().map(|output| &output.state));
        self.status_label.set_label(status.label());
        for class in ["dim-label", "warning", "error", "success"] {
            self.status_label.remove_css_class(class);
        }
        self.status_label.add_css_class(status.css_class());
    }

    /// Syncs the transition selector from the snapshot without re-dispatching.
    fn sync_transition(&self, snapshot: &AppSnapshot) {
        if let Some(index) = choice_for_transition(snapshot.state().transition.kind) {
            if self.transition_dropdown.selected() != index {
                self.syncing_transition.set(true);
                self.transition_dropdown.set_selected(index);
                self.syncing_transition.set(false);
            }
        }
    }

    /// Shows a dialog asking for a new scene's name.
    fn present_add_scene_dialog(root: &adw::ApplicationWindow, sender: relm4::Sender<AppMsg>) {
        let (dialog, entry, add_button) = name_dialog("Add Scene", "Scene name");
        let submit = {
            let dialog = dialog.clone();
            move || {
                let name = entry.text().trim().to_string();
                if !name.is_empty() {
                    sender.emit(AppMsg::AddSceneSubmitted(name));
                    dialog.close();
                }
            }
        };
        add_button.connect_clicked({
            let submit = submit.clone();
            move |_| submit()
        });
        dialog.present(Some(root));
    }

    /// Shows a dialog asking for a new source's kind and name.
    fn present_add_source_dialog(root: &adw::ApplicationWindow, sender: relm4::Sender<AppMsg>) {
        let dialog = adw::Dialog::new();
        dialog.set_title("Add Source");
        dialog.set_content_width(360);

        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        let add_button = gtk::Button::with_label("Add");
        add_button.add_css_class("suggested-action");
        header.pack_end(&add_button);
        toolbar.add_top_bar(&header);

        let name_row = adw::EntryRow::new();
        name_row.set_title("Source name");
        let kind_row = adw::ComboRow::new();
        kind_row.set_title("Kind");
        let labels: Vec<&str> = SOURCE_KIND_CHOICES
            .iter()
            .map(|(_, label)| *label)
            .collect();
        kind_row.set_model(Some(&gtk::StringList::new(&labels)));

        let group = adw::PreferencesGroup::new();
        group.add(&kind_row);
        group.add(&name_row);
        let clamp = adw::Clamp::new();
        clamp.set_margin_start(12);
        clamp.set_margin_end(12);
        clamp.set_margin_top(12);
        clamp.set_margin_bottom(12);
        clamp.set_child(Some(&group));
        toolbar.set_content(Some(&clamp));
        dialog.set_child(Some(&toolbar));

        name_row.connect_changed({
            let add_button = add_button.clone();
            move |entry| add_button.set_sensitive(!entry.text().trim().is_empty())
        });
        add_button.set_sensitive(!name_row.text().trim().is_empty());

        let submit = {
            let dialog = dialog.clone();
            let name_row = name_row.clone();
            let kind_row = kind_row.clone();
            move || {
                let name = name_row.text().trim().to_string();
                let index = usize::try_from(kind_row.selected()).unwrap_or(0);
                let kind = SOURCE_KIND_CHOICES
                    .get(index)
                    .map(|(kind, _)| *kind)
                    .unwrap_or(SourceKind::TestPattern);
                if !name.is_empty() {
                    sender.emit(AppMsg::AddSourceSubmitted { kind, name });
                    dialog.close();
                }
            }
        };
        add_button.connect_clicked({
            let submit = submit.clone();
            move |_| submit()
        });
        name_row.connect_entry_activated(move |_| submit());
        dialog.present(Some(root));
    }
}

/// Builds an `adw::Dialog` with a single name entry and an "Add" button in
/// the header. The button starts insensitive and tracks the entry text.
fn name_dialog(title: &str, entry_title: &str) -> (adw::Dialog, adw::EntryRow, gtk::Button) {
    let dialog = adw::Dialog::new();
    dialog.set_title(title);
    dialog.set_content_width(360);

    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    let add_button = gtk::Button::with_label("Add");
    add_button.add_css_class("suggested-action");
    header.pack_end(&add_button);
    toolbar.add_top_bar(&header);

    let entry = adw::EntryRow::new();
    entry.set_title(entry_title);
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

    add_button.set_sensitive(!entry.text().trim().is_empty());
    entry.connect_changed({
        let add_button = add_button.clone();
        move |entry| add_button.set_sensitive(!entry.text().trim().is_empty())
    });

    (dialog, entry, add_button)
}

impl AsyncComponent for AppModel {
    type Init = CoreBridge;
    type Input = AppMsg;
    type Output = ();
    type CommandOutput = AppCmd;
    type Root = adw::ApplicationWindow;
    type Widgets = ();

    fn init_root() -> Self::Root {
        adw::ApplicationWindow::new(&relm4::main_adw_application())
    }

    async fn init(
        bridge: Self::Init,
        root: Self::Root,
        sender: AsyncComponentSender<Self>,
    ) -> AsyncComponentParts<Self> {
        root.set_title(Some("Prismcast"));
        root.set_default_size(1280, 800);

        // --- Header bar: app name, profile/collection, stream status ---
        let title_box = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let app_label = gtk::Label::new(Some("Prismcast"));
        app_label.add_css_class("title");
        let profile_label = gtk::Label::new(None);
        profile_label.add_css_class("dim-label");
        let collection_label = gtk::Label::new(None);
        collection_label.add_css_class("dim-label");
        title_box.append(&app_label);
        title_box.append(&profile_label);
        title_box.append(&collection_label);

        let status_label = gtk::Label::new(Some("Offline"));
        status_label.add_css_class("dim-label");
        status_label.set_margin_end(6);

        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&title_box));
        header.pack_end(&status_label);

        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_top_bar(&header);

        // --- Central preview placeholder (real paintable: MEDIA-004) ---
        let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
        content.set_margin_start(6);
        content.set_margin_end(6);
        content.set_margin_top(6);
        content.set_margin_bottom(6);

        let preview = gtk::Frame::new(Some("Preview"));
        preview.set_vexpand(true);
        preview.add_css_class("view");
        let preview_page = adw::StatusPage::new();
        preview_page.set_title("Preview");
        preview_page.set_description(Some(
            "The live compositor paintable arrives with MEDIA-004.",
        ));
        preview.set_child(Some(&preview_page));
        content.append(&preview);

        // --- Panels: scenes | sources | audio mixer placeholder ---
        let scenes = ScenesPanel::builder()
            .launch(())
            .forward(sender.input_sender(), |message| match message {
                ScenesOutput::Select(scene_id) => AppMsg::SelectScene(scene_id),
                ScenesOutput::AddRequested => AppMsg::AddSceneRequested,
            });
        let sources =
            SourcesPanel::builder()
                .launch(())
                .forward(sender.input_sender(), |message| match message {
                    SourcesOutput::AddRequested => AppMsg::AddSourceRequested,
                });
        let outputs =
            OutputsPanel::builder()
                .launch(())
                .forward(sender.input_sender(), |message| match message {
                    OutputsOutput::StartRequested(output_id) => AppMsg::StartOutput(output_id),
                    OutputsOutput::StopRequested(output_id) => AppMsg::StopOutput(output_id),
                    OutputsOutput::AddRequested => AppMsg::AddOutputRequested,
                });

        let panels = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        panels.set_height_request(220);
        scenes.widget().set_width_request(200);
        sources.widget().set_hexpand(true);
        panels.append(scenes.widget());
        panels.append(sources.widget());

        let mixer = gtk::Frame::new(Some("Audio Mixer"));
        mixer.set_width_request(260);
        let mixer_label = gtk::Label::new(Some("Mixer meters arrive with the audio graph."));
        mixer_label.set_wrap(true);
        mixer_label.set_max_width_chars(24);
        mixer_label.add_css_class("dim-label");
        mixer.set_child(Some(&mixer_label));
        panels.append(&mixer);
        content.append(&panels);

        // --- Bottom bar: transition selector + outputs/controls ---
        let bottom_bar = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let transition_label = gtk::Label::new(Some("Transition:"));
        let transition_dropdown = gtk::DropDown::from_strings(&["Cut", "Fade", "Swipe", "Slide"]);
        bottom_bar.append(&transition_label);
        bottom_bar.append(&transition_dropdown);
        bottom_bar.append(outputs.widget());
        content.append(&bottom_bar);

        let toast_overlay = adw::ToastOverlay::new();
        toast_overlay.set_child(Some(&content));
        toolbar_view.set_content(Some(&toast_overlay));
        root.set_content(Some(&toolbar_view));

        // Transition selector → SetTransition command.
        {
            let input = sender.input_sender().clone();
            transition_dropdown.connect_selected_notify(move |dropdown| {
                input.emit(AppMsg::TransitionSelected(dropdown.selected()));
            });
        }

        // Window close → graceful actor shutdown, then actually close. The
        // flag lives solely in the closure: once set (first close attempt),
        // the final close after `FinishShutdown` is allowed through.
        {
            let shutdown_started = Rc::new(Cell::new(false));
            let handle = bridge.handle().clone();
            let input = sender.input_sender().clone();
            root.connect_close_request(move |_| {
                if shutdown_started.get() {
                    return gtk::glib::Propagation::Proceed;
                }
                shutdown_started.set(true);
                let handle = handle.clone();
                let input = input.clone();
                relm4::spawn_local(async move {
                    handle.shutdown().await;
                    input.emit(AppMsg::FinishShutdown);
                });
                gtk::glib::Propagation::Stop
            });
        }

        // Core event stream → AppMsg::Pump on the GTK main loop.
        bridge.spawn_event_pump(sender.input_sender().clone(), AppMsg::Pump);

        let model = Self {
            bridge,
            scenes,
            sources,
            outputs,
            profile_label,
            collection_label,
            status_label,
            transition_dropdown,
            toast_overlay,
            syncing_transition: Cell::new(false),
        };
        model.publish_snapshot();
        AsyncComponentParts { model, widgets: () }
    }

    async fn update(
        &mut self,
        message: Self::Input,
        sender: AsyncComponentSender<Self>,
        root: &Self::Root,
    ) {
        match message {
            AppMsg::Pump(pump) => match pump {
                PumpEvent::Event { seq, event } => {
                    debug!(seq, ?event, "core event");
                    self.publish_snapshot();
                }
                PumpEvent::Lagged { dropped } => {
                    warn!(dropped, "event stream lagged; resyncing from snapshot");
                    self.publish_snapshot();
                }
                PumpEvent::Closed => {
                    info!("core event stream closed");
                }
            },
            AppMsg::SelectScene(scene_id) => {
                self.dispatch(&sender, Command::SetCurrentScene { scene_id });
            }
            AppMsg::AddSceneRequested => {
                Self::present_add_scene_dialog(root, sender.input_sender().clone());
            }
            AppMsg::AddSceneSubmitted(name) => {
                self.dispatch(&sender, Command::AddScene { name });
            }
            AppMsg::AddSourceRequested => {
                Self::present_add_source_dialog(root, sender.input_sender().clone());
            }
            AppMsg::AddSourceSubmitted { kind, name } => {
                self.dispatch(&sender, Command::AddSource { kind, name });
            }
            AppMsg::AddOutputRequested => {
                let output = Output::new(OutputKind::Recording, "Recording", EncoderId::new());
                self.dispatch(&sender, Command::AddOutput { output });
            }
            AppMsg::StartOutput(output_id) => {
                self.dispatch(&sender, Command::StartOutput { output_id });
            }
            AppMsg::StopOutput(output_id) => {
                self.dispatch(&sender, Command::StopOutput { output_id });
            }
            AppMsg::TransitionSelected(index) => {
                if self.syncing_transition.get() {
                    return;
                }
                if let Some(transition) = transition_for_choice(index) {
                    self.dispatch(&sender, Command::SetTransition { transition });
                }
            }
            AppMsg::FinishShutdown => {
                info!("core actor stopped; closing window");
                root.close();
            }
        }
    }

    async fn update_cmd(
        &mut self,
        message: Self::CommandOutput,
        _sender: AsyncComponentSender<Self>,
        _root: &Self::Root,
    ) {
        match message {
            AppCmd::Dispatched(Ok(response)) => {
                debug!(
                    label = response.label,
                    events = response.events.len(),
                    "command applied"
                );
            }
            AppCmd::Dispatched(Err(error)) => {
                warn!(%error, "command rejected");
                self.toast_overlay
                    .add_toast(adw::Toast::new(&format!("Command failed: {error}")));
            }
        }
    }
}
