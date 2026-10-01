//! The root Relm4 component: main window layout per PLAN.md §28 and the only
//! place that talks to the core.
//!
//! Every user action becomes a [`Command`] dispatched through
//! [`prismcast_app::AppHandle`] (as a Relm4 *command* whose result lands in
//! [`AppModel::update_cmd`]); every UI update flows from the core's
//! snapshot watch — coalesced into [`AppMsg::Pump`] by [`CoreBridge`] — which
//! triggers latest [`AppSnapshot`] reads in the root and panels. The UI never mutates
//! domain state directly (AGENTS.md central invariant, PLAN.md §76).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use prismcast_app::{AppSnapshot, CommandResponse, HandleError};
use prismcast_core::id::{EncoderId, OutputId, SceneId};
use prismcast_core::output::{Output, OutputKind};
use prismcast_core::source::SourceKind;
use prismcast_core::Command;
use prismcast_preview::{PreviewSession, PreviewStatus};
use relm4::component::{AsyncComponent, AsyncComponentParts};
use relm4::{AsyncComponentSender, Component, ComponentController, Controller};
use tracing::{debug, info, warn};

use crate::bridge::{CoreBridge, SnapshotRefresh};
use crate::components::outputs::{OutputsInput, OutputsOutput, OutputsPanel};
use crate::components::scenes::{ScenesInput, ScenesOutput, ScenesPanel};
use crate::components::sources::{SourcesInput, SourcesOutput, SourcesPanel};
use crate::presentation::{choice_for_transition, transition_for_choice, StreamStatus};
use crate::preview_editor::PreviewEditor;

/// Kinds offered by the add-source dialog, in picker order.
const SOURCE_KIND_CHOICES: [(SourceKind, &str); 4] = [
    (SourceKind::TestPattern, "Test Pattern"),
    (SourceKind::Color, "Solid Color"),
    (SourceKind::PipeWireDisplay, "Monitor Capture"),
    (SourceKind::PipeWireWindow, "Window Capture"),
];

/// Root component inputs: user intents plus coalesced snapshot wakeups.
#[derive(Debug)]
pub enum AppMsg {
    /// A latest-snapshot wakeup (sent cross-thread by the pump).
    Pump(SnapshotRefresh),
    /// The user selected a scene in the scenes panel.
    SelectScene(SceneId),
    /// Scene edit forwarded to the shared command dispatcher.
    SceneCommand(Command),
    SourceCommand(Box<Command>),
    AuthorizeCapture(prismcast_core::SourceId),
    CaptureParentReady(Result<(), String>),
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
        /// Scene captured when the dialog was opened.
        scene_id: SceneId,
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
    BeginShutdown,
    PreviewWake,
    PreviewCommand(Box<Command>),
}

/// Results of commands dispatched to the core actor.
#[derive(Debug)]
pub enum AppCmd {
    /// A command dispatch returned (success or rejection).
    Dispatched(Result<CommandResponse, HandleError>),
    PreviewDispatched(Result<CommandResponse, HandleError>),
    CaptureDispatched(Result<CommandResponse, HandleError>),
    /// Creating a shared source succeeded but placing it failed.
    PlacementFailed {
        source_id: prismcast_core::SourceId,
        error: HandleError,
    },
    SourceEventMissing,
}

/// The root application model. Holds the core bridge, the panel controllers,
/// and the widgets it updates directly — presentation state only.
pub struct AppModel {
    capture_parent: Rc<RefCell<Option<crate::capture_parent::CaptureParent>>>,
    capture_parent_pending: bool,
    capture_dispatch_pending: bool,
    shutting_down: Rc<Cell<bool>>,
    bridge: CoreBridge,
    preview_session: Option<PreviewSession>,
    preview_editor: PreviewEditor,
    preview_status: Option<tokio::sync::watch::Receiver<PreviewStatus>>,
    preview_pump: Option<gtk::glib::JoinHandle<()>>,
    preview_pending: Rc<Cell<bool>>,
    preview_stack: gtk::Stack,
    preview_message: gtk::Label,
    shutdown_complete: Rc<Cell<bool>>,
    snapshot_pump: tokio::task::JoinHandle<()>,
    scene_refresh: SnapshotRefresh,
    source_refresh: SnapshotRefresh,
    output_refresh: SnapshotRefresh,
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
    syncing_transition: Rc<Cell<bool>>,
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
        self.scene_refresh.notify(|wake| {
            self.scenes.emit(ScenesInput::Refresh(wake));
            true
        });
        self.source_refresh.notify(|wake| {
            self.sources.emit(SourcesInput::Refresh(wake));
            true
        });
        self.output_refresh.notify(|wake| {
            self.outputs.emit(OutputsInput::Refresh(wake));
            true
        });
        self.preview_editor.refresh(snapshot.clone());
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
    fn present_add_source_dialog(
        root: &adw::ApplicationWindow,
        sender: relm4::Sender<AppMsg>,
        scene_id: SceneId,
    ) {
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
                    sender.emit(AppMsg::AddSourceSubmitted {
                        kind,
                        name,
                        scene_id,
                    });
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

        // GTK-local presentation supplied by the dedicated preview adapter.
        let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
        content.set_margin_start(6);
        content.set_margin_end(6);
        content.set_margin_top(6);
        content.set_margin_bottom(6);
        let preview = gtk::Frame::new(Some("Program Preview"));
        preview.set_vexpand(true);
        preview.add_css_class("view");
        let preview_stack = gtk::Stack::new();
        let picture = gtk::Picture::new();
        picture.set_content_fit(gtk::ContentFit::Contain);
        picture.set_can_shrink(true);
        let preview_message = gtk::Label::new(Some("Starting preview…"));
        preview_message.set_wrap(true);
        preview_message.set_max_width_chars(80);
        let preview_editor = PreviewEditor::new(
            &picture,
            {
                let handle = bridge.handle().clone();
                move || handle.snapshot()
            },
            {
                let input = sender.input_sender().clone();
                move |command| input.emit(AppMsg::PreviewCommand(Box::new(command)))
            },
        );
        preview_stack.add_named(preview_editor.widget(), Some("video"));
        preview_stack.add_named(&preview_message, Some("status"));
        preview_stack.set_visible_child_name("status");
        preview.set_child(Some(&preview_stack));
        content.append(&preview);
        let preview_session = match PreviewSession::start(bridge.handle().clone()) {
            Ok(session) => {
                picture.set_paintable(Some(session.paintable()));
                Some(session)
            }
            Err(error) => {
                warn!(%error, "preview attachment failed");
                preview_message.set_label(&format!("Preview unavailable: {error}"));
                None
            }
        };
        let preview_status = preview_session
            .as_ref()
            .map(PreviewSession::subscribe_status);
        let preview_pending = Rc::new(Cell::new(false));
        let preview_pump = preview_status.as_ref().map(|status| {
            let mut status = status.clone();
            let pending = preview_pending.clone();
            let input = sender.input_sender().clone();
            relm4::spawn_local(async move {
                while status.changed().await.is_ok() {
                    if !pending.replace(true) && input.send(AppMsg::PreviewWake).is_err() {
                        break;
                    }
                }
            })
        });

        // --- Panels: scenes | sources | audio mixer placeholder ---
        let scenes = ScenesPanel::builder()
            .launch(())
            .forward(sender.input_sender(), |message| match message {
                ScenesOutput::Select(scene_id) => AppMsg::SelectScene(scene_id),
                ScenesOutput::AddRequested => AppMsg::AddSceneRequested,
                ScenesOutput::Command(command) => AppMsg::SceneCommand(*command),
            });
        let sources =
            SourcesPanel::builder()
                .launch(())
                .forward(sender.input_sender(), |message| match message {
                    SourcesOutput::AddRequested => AppMsg::AddSourceRequested,
                    SourcesOutput::Command(command) => AppMsg::SourceCommand(command),
                    SourcesOutput::AuthorizeCapture(source_id) => {
                        AppMsg::AuthorizeCapture(source_id)
                    }
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
        // A single GTK-local export per window. This never opens a picker.
        let capture_parent = Rc::new(RefCell::new(None));
        let shutting_down = Rc::new(Cell::new(false));
        root.connect_realize({
            let parent = Rc::downgrade(&capture_parent);
            let shutting_down = shutting_down.clone();
            let input = sender.input_sender().clone();
            let exported = Cell::new(false);
            move |window| {
                if !exported.replace(true) {
                    let input = input.clone();
                    let parent = parent.clone();
                    let shutting_down = shutting_down.clone();
                    crate::capture_parent::export(window, move |result| {
                        let result = result.map(|export| {
                            if !shutting_down.get() {
                                if let Some(parent) = parent.upgrade() {
                                    *parent.borrow_mut() = Some(export);
                                }
                            }
                        });
                        input.emit(AppMsg::CaptureParentReady(result));
                    });
                }
            }
        });

        let syncing_transition = Rc::new(Cell::new(false));
        // Suppress rendering notifications in the signal callback, before
        // they become queued messages and outlive the synchronization flag.
        {
            let input = sender.input_sender().clone();
            let syncing = Rc::clone(&syncing_transition);
            transition_dropdown.connect_selected_notify(move |dropdown| {
                if !syncing.get() {
                    input.emit(AppMsg::TransitionSelected(dropdown.selected()));
                }
            });
        }

        // Repeated close requests remain stopped until both owners finish.
        let shutdown_complete = Rc::new(Cell::new(false));
        {
            let shutdown_started = Rc::new(Cell::new(false));
            let finished = shutdown_complete.clone();
            let input = sender.input_sender().clone();
            root.connect_close_request(move |_| {
                if finished.get() {
                    return gtk::glib::Propagation::Proceed;
                }
                if !shutdown_started.replace(true) {
                    input.emit(AppMsg::BeginShutdown);
                }
                gtk::glib::Propagation::Stop
            });
        }

        // Committed snapshot publication → coalesced GTK wakeup.
        let snapshot_pump = bridge.spawn_snapshot_pump(sender.input_sender().clone(), AppMsg::Pump);
        let scene_refresh = SnapshotRefresh::new(bridge.handle().clone());
        let source_refresh = SnapshotRefresh::new(bridge.handle().clone());
        let output_refresh = SnapshotRefresh::new(bridge.handle().clone());

        let model = Self {
            capture_parent,
            capture_parent_pending: true,
            capture_dispatch_pending: false,
            shutting_down,
            bridge,
            preview_session,
            preview_editor,
            preview_status,
            preview_pump,
            preview_pending,
            preview_stack,
            preview_message,
            shutdown_complete,
            snapshot_pump,
            scene_refresh,
            source_refresh,
            output_refresh,
            scenes,
            sources,
            outputs,
            profile_label,
            collection_label,
            status_label,
            transition_dropdown,
            toast_overlay,
            syncing_transition,
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
            AppMsg::Pump(refresh) => {
                // Acknowledge before reading; watch changes racing this read
                // either appear now or leave another refresh queued.
                let _ = refresh.read();
                self.publish_snapshot();
            }
            AppMsg::SceneCommand(command) => self.dispatch(&sender, command),
            AppMsg::SelectScene(scene_id) => {
                self.dispatch(&sender, Command::SetCurrentScene { scene_id });
            }
            AppMsg::AddSceneRequested => {
                Self::present_add_scene_dialog(root, sender.input_sender().clone());
            }
            AppMsg::AddSceneSubmitted(name) => {
                self.dispatch(&sender, Command::AddScene { name });
            }
            AppMsg::SourceCommand(command) => self.dispatch(&sender, *command),
            AppMsg::CaptureParentReady(result) => {
                self.capture_parent_pending = false;
                match result {
                    Ok(()) => {}
                    Err(error) => warn!(%error, "portal parent export unavailable"),
                }
            }
            AppMsg::AuthorizeCapture(source_id) => {
                if self.shutting_down.get() || self.capture_dispatch_pending {
                    self.publish_snapshot();
                    return;
                }
                if self.capture_parent_pending {
                    self.toast_overlay.add_toast(adw::Toast::new(
                        "The window is preparing capture authorization. Try again shortly.",
                    ));
                    self.publish_snapshot();
                    return;
                }
                let snapshot = self.bridge.handle().snapshot();
                if !snapshot.source(source_id).is_some_and(|source| {
                    source.enabled
                        && matches!(
                            source.kind,
                            SourceKind::PipeWireDisplay | SourceKind::PipeWireWindow
                        )
                }) || snapshot.source_runtime(source_id).is_some_and(|runtime| {
                    matches!(
                        runtime.status,
                        prismcast_core::CaptureStatus::Authorizing
                            | prismcast_core::CaptureStatus::Active
                    )
                }) {
                    self.publish_snapshot();
                    return;
                }
                self.capture_dispatch_pending = true;
                let parent = self
                    .capture_parent
                    .borrow()
                    .as_ref()
                    .map(|parent| parent.identifier());
                let handle = self.bridge.handle().clone();
                sender.oneshot_command(async move {
                    AppCmd::CaptureDispatched(
                        handle.authorize_source_capture(source_id, parent).await,
                    )
                });
            }
            AppMsg::AddSourceRequested => {
                if let Some(scene_id) = self.bridge.handle().snapshot().current_scene() {
                    Self::present_add_source_dialog(root, sender.input_sender().clone(), scene_id);
                } else {
                    self.toast_overlay.add_toast(adw::Toast::new(
                        "Add or select a scene before creating a source.",
                    ));
                }
            }
            AppMsg::AddSourceSubmitted {
                kind,
                name,
                scene_id,
            } => {
                let handle = self.bridge.handle().clone();
                sender.oneshot_command(async move {
                    let response = match handle.dispatch(Command::AddSource { kind, name }).await {
                        Ok(response) => response,
                        Err(error) => return AppCmd::Dispatched(Err(error)),
                    };
                    let Some(source_id) = created_source_id(&response.events) else {
                        return AppCmd::SourceEventMissing;
                    };
                    match handle
                        .dispatch(Command::AddSceneItem {
                            scene_id,
                            source_id,
                        })
                        .await
                    {
                        Ok(response) => AppCmd::Dispatched(Ok(response)),
                        Err(error) => AppCmd::PlacementFailed { source_id, error },
                    }
                });
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
            AppMsg::PreviewCommand(command) => {
                let handle = self.bridge.handle().clone();
                sender.oneshot_command(async move {
                    AppCmd::PreviewDispatched(handle.dispatch(*command).await)
                });
            }
            AppMsg::PreviewWake => {
                self.preview_pending.set(false);
                let status = self
                    .preview_status
                    .as_mut()
                    .map(|status| status.borrow_and_update().clone());
                self.preview_editor.set_available(matches!(
                    status,
                    Some(PreviewStatus::Running | PreviewStatus::Degraded(_))
                ));
                match status {
                    Some(PreviewStatus::Running) => {
                        self.preview_stack.set_visible_child_name("video")
                    }
                    Some(PreviewStatus::Degraded(message)) => {
                        self.preview_stack.set_visible_child_name("video");
                        self.toast_overlay
                            .add_toast(adw::Toast::new(&format!("Preview warning: {message}")));
                    }
                    Some(PreviewStatus::Failed(message)) => {
                        self.preview_message
                            .set_label(&format!("Preview unavailable: {message}"));
                        self.preview_stack.set_visible_child_name("status");
                    }
                    Some(PreviewStatus::Stopped) => {
                        self.preview_message.set_label("Preview stopped");
                        self.preview_stack.set_visible_child_name("status");
                    }
                    _ => {}
                }
            }
            AppMsg::BeginShutdown => {
                self.shutting_down.set(true);
                let preview = self.preview_session.take();
                let parent = self.capture_parent.borrow_mut().take();
                let handle = self.bridge.handle().clone();
                let input = sender.input_sender().clone();
                relm4::spawn_local(async move {
                    if let Some(preview) = preview {
                        if let Err(error) = preview.shutdown().await {
                            warn!(%error, "preview shutdown failed");
                        }
                    }
                    // The GTK-local export outlives pending portal/native work,
                    // even if the component disappears during awaited shutdown.
                    drop(parent);
                    handle.shutdown().await;
                    input.emit(AppMsg::FinishShutdown);
                });
            }
            AppMsg::FinishShutdown => {
                self.capture_parent.borrow_mut().take();
                self.shutdown_complete.set(true);
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
            AppCmd::CaptureDispatched(result) => {
                self.capture_dispatch_pending = false;
                if let Err(error) = result {
                    self.toast_overlay.add_toast(adw::Toast::new(&format!(
                        "Capture authorization failed: {error}"
                    )));
                }
                self.publish_snapshot();
            }
            AppCmd::PreviewDispatched(result) => {
                self.preview_editor
                    .completed(self.bridge.handle().snapshot());
                if let Err(error) = result {
                    self.toast_overlay
                        .add_toast(adw::Toast::new(&format!("Preview edit failed: {error}")));
                }
            }
            AppCmd::PlacementFailed { source_id, error } => {
                warn!(%source_id, %error, "shared source created but placement failed");
                self.toast_overlay.add_toast(adw::Toast::new(&format!("Source was created, but could not be placed: {error}. Place it from Shared sources.")));
            }
            AppCmd::SourceEventMissing => {
                self.toast_overlay.add_toast(adw::Toast::new("Source creation returned no source ID; inspect Shared sources before retrying."));
            }
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

impl Drop for AppModel {
    fn drop(&mut self) {
        self.snapshot_pump.abort();
        if let Some(pump) = self.preview_pump.take() {
            pump.abort();
        }
    }
}

fn created_source_id(events: &[prismcast_core::Event]) -> Option<prismcast_core::SourceId> {
    events.iter().find_map(|event| match event {
        prismcast_core::Event::Source(prismcast_core::SourceEvent::Added { source }) => {
            Some(source.id)
        }
        _ => None,
    })
}

#[cfg(test)]
mod source_placement_tests {
    #[test]
    fn source_dialog_offers_both_capture_kinds_without_authorizing() {
        assert!(super::SOURCE_KIND_CHOICES
            .iter()
            .any(|(kind, _)| matches!(kind, prismcast_core::SourceKind::PipeWireDisplay)));
        assert!(super::SOURCE_KIND_CHOICES
            .iter()
            .any(|(kind, _)| matches!(kind, prismcast_core::SourceKind::PipeWireWindow)));
    }
    use super::*;
    #[test]
    fn uses_committed_source_identity_and_handles_missing_creation_event() {
        let source = prismcast_core::Source::new(SourceKind::TestPattern, "Pattern");
        let id = source.id;
        assert_eq!(created_source_id(&[]), None);
        assert_eq!(
            created_source_id(&[prismcast_core::Event::Source(
                prismcast_core::SourceEvent::Added {
                    source: Box::new(source)
                }
            )]),
            Some(id)
        );
    }
    #[test]
    fn placement_uses_captured_scene_and_preserves_source_on_missing_scene() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let handle = prismcast_app::AppHandle::spawn(prismcast_app::CoreConfig::default());
            handle
                .dispatch(Command::AddScene {
                    name: "Target".into(),
                })
                .await
                .unwrap();
            let target = handle.snapshot().scenes().next().unwrap().id;
            handle
                .dispatch(Command::AddScene {
                    name: "Other".into(),
                })
                .await
                .unwrap();
            let other = handle
                .snapshot()
                .scenes()
                .find(|scene| scene.id != target)
                .unwrap()
                .id;
            handle
                .dispatch(Command::SetCurrentScene { scene_id: other })
                .await
                .unwrap();
            let response = handle
                .dispatch(Command::AddSource {
                    kind: SourceKind::TestPattern,
                    name: "Pattern".into(),
                })
                .await
                .unwrap();
            let source_id = created_source_id(&response.events).unwrap();
            handle
                .dispatch(Command::AddSceneItem {
                    scene_id: target,
                    source_id,
                })
                .await
                .unwrap();
            assert_eq!(
                handle.snapshot().state().scenes[&target].items[0].source_id,
                source_id
            );
            assert!(handle.snapshot().state().scenes[&other].items.is_empty());
            handle
                .dispatch(Command::RemoveScene { scene_id: target })
                .await
                .unwrap();
            assert!(handle
                .dispatch(Command::AddSceneItem {
                    scene_id: target,
                    source_id
                })
                .await
                .is_err());
            assert!(handle.snapshot().state().sources.contains_key(&source_id));
            handle.shutdown().await;
        });
    }
}

#[cfg(test)]
mod shell_display_tests {
    use super::*;
    use std::{
        cell::RefCell,
        time::{Duration, Instant},
    };

    fn picture(widget: &gtk::Widget) -> Option<gtk::Picture> {
        if let Ok(picture) = widget.clone().downcast::<gtk::Picture>() {
            return Some(picture);
        }
        let mut child = widget.first_child();
        while let Some(widget) = child {
            if let Some(picture) = picture(&widget) {
                return Some(picture);
            }
            child = widget.next_sibling();
        }
        None
    }

    async fn exercise(
        app: &adw::Application,
        handle: &prismcast_app::AppHandle,
    ) -> Result<(), String> {
        let profile = prismcast_core::Profile::new(
            "Shell test",
            prismcast_core::VideoConfig {
                width: 1280,
                height: 720,
                fps_num: 30,
                fps_den: 1,
            },
        );
        let profile_id = profile.id;
        handle
            .dispatch(Command::AddProfile { profile })
            .await
            .map_err(|error| error.to_string())?;
        handle
            .dispatch(Command::SelectProfile { profile_id })
            .await
            .map_err(|error| error.to_string())?;
        handle
            .dispatch(Command::AddScene {
                name: "Shell smoke".into(),
            })
            .await
            .map_err(|error| error.to_string())?;
        let scene_id = handle
            .snapshot()
            .current_scene()
            .ok_or("scene not committed")?;
        let response = handle
            .dispatch(Command::AddSource {
                kind: SourceKind::TestPattern,
                name: "Shell pattern".into(),
            })
            .await
            .map_err(|error| error.to_string())?;
        let source_id = created_source_id(&response.events).ok_or("source ID missing")?;
        handle
            .dispatch(Command::SetSourceSettings {
                source_id,
                settings: serde_json::json!({"width":1280,"height":720,"fps":30,"pattern":"red"}),
            })
            .await
            .map_err(|error| error.to_string())?;
        handle
            .dispatch(Command::AddSceneItem {
                scene_id,
                source_id,
            })
            .await
            .map_err(|error| error.to_string())?;
        let deadline = Instant::now() + Duration::from_secs(10);
        let (window, paintable) = loop {
            if let Some(window) = app.active_window() {
                if let Some(picture) = picture(window.upcast_ref()) {
                    if let Some(paintable) = picture.paintable() {
                        if paintable.intrinsic_width() == 1280
                            && paintable.intrinsic_height() == 720
                        {
                            break (window, paintable);
                        }
                    }
                }
            }
            if Instant::now() > deadline {
                return Err("native shell preview never received a frame".into());
            }
            gtk::glib::timeout_future(Duration::from_millis(30)).await;
        };
        let invalidations = Rc::new(Cell::new(0));
        paintable.connect_invalidate_contents({
            let count = invalidations.clone();
            move |_| count.set(count.get() + 1)
        });
        let item_id = handle.snapshot().state().scenes[&scene_id].items[0].id;
        handle
            .dispatch(Command::SetSceneItemVisible {
                scene_id,
                item_id,
                visible: false,
            })
            .await
            .map_err(|error| error.to_string())?;
        gtk::glib::timeout_future(Duration::from_millis(200)).await;
        if invalidations.get() == 0 {
            return Err("shell paintable stopped updating after a command".into());
        }
        // Exercise the actual production callback, including repeated close.
        window.close();
        window.close();
        gtk::glib::future_with_timeout(Duration::from_secs(5), handle.closed())
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    #[test]
    #[ignore = "requires a real GTK display; run this filter with --ignored --test-threads=1"]
    fn native_shell_preview_and_window_close_stop_both_owners() {
        let (bridge, core_thread) = CoreBridge::spawn_background().unwrap();
        let handle = bridge.handle().clone();
        let relm_app = relm4::RelmApp::new("io.github.worxbend.prismcast.smoketest")
            .with_args(vec!["prismcast-smoketest".into()]);
        relm_app.allow_multiple_instances(true);
        let app = relm4::main_adw_application();
        let result = Rc::new(RefCell::new(None));
        app.connect_activate({
            let result = result.clone();
            let handle = handle.clone();
            move |app| {
                let app = app.clone();
                let result = result.clone();
                let handle = handle.clone();
                relm4::spawn_local(async move {
                    let outcome = exercise(&app, &handle).await;
                    let failed = outcome.is_err();
                    *result.borrow_mut() = Some(outcome);
                    if failed {
                        handle.shutdown().await;
                        app.quit();
                    }
                });
            }
        });
        relm_app.run_async::<AppModel>(bridge);
        core_thread.join().unwrap();
        assert_eq!(result.borrow_mut().take(), Some(Ok(())));
    }
}
