//! Audio controls send core commands; meters render measured observations only.
use crate::bridge::{MeterRefresh, SnapshotRefresh};
use adw::prelude::*;
use prismcast_app::{AppSnapshot, MeterSnapshot};
use prismcast_core::{Command, SourceId, SourceKind};
use relm4::{ComponentParts, ComponentSender, SimpleComponent};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

pub struct AudioPanel {
    cards: gtk::Box,
    empty: gtk::Label,
    health: gtk::Label,
    rows: BTreeMap<SourceId, AudioRow>,
}
#[derive(Debug)]
pub enum AudioInput {
    Refresh(SnapshotRefresh),
    Meters(MeterRefresh),
    Status(prismcast_preview::AudioStatus),
    StatusWake(AudioStatusRefresh),
}
/// Latest health notification with the same finite notification budget as meters.
#[derive(Clone)]
pub struct AudioStatusRefresh {
    status: tokio::sync::watch::Receiver<prismcast_preview::AudioStatus>,
    pending: Arc<AtomicBool>,
}
impl std::fmt::Debug for AudioStatusRefresh {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioStatusRefresh").finish_non_exhaustive()
    }
}
impl AudioStatusRefresh {
    pub fn new(status: tokio::sync::watch::Receiver<prismcast_preview::AudioStatus>) -> Self {
        Self {
            status,
            pending: Arc::new(AtomicBool::new(false)),
        }
    }
    pub fn notify(&self, sender: &relm4::Sender<AudioInput>) -> bool {
        if self.pending.swap(true, Ordering::AcqRel) {
            return true;
        }
        if sender.send(AudioInput::StatusWake(self.clone())).is_ok() {
            true
        } else {
            self.pending.store(false, Ordering::Release);
            false
        }
    }
    fn read(&self) -> prismcast_preview::AudioStatus {
        self.pending.store(false, Ordering::Release);
        self.status.borrow().clone()
    }
}
#[derive(Debug)]
pub enum AudioOutput {
    Command(Box<Command>),
    AddTestTone,
}
struct AudioRow {
    widget: gtk::Box,
    name: gtk::Label,
    route: gtk::Label,
    gain: gtk::Scale,
    muted: gtk::CheckButton,
    solo: gtk::CheckButton,
    level: gtk::LevelBar,
    reading: gtk::Label,
    restoring: Rc<Cell<bool>>,
    dragging: Rc<Cell<bool>>,
    remove: gtk::Button,
    removal: Rc<RefCell<Option<Command>>>,
}
impl SimpleComponent for AudioPanel {
    type Input = AudioInput;
    type Output = AudioOutput;
    type Init = ();
    type Root = gtk::Frame;
    type Widgets = ();
    fn init_root() -> Self::Root {
        gtk::Frame::new(Some("Audio Mixer"))
    }
    fn init(_: (), root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        root.set_width_request(310);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
        content.set_margin_start(6);
        content.set_margin_end(6);
        content.set_margin_top(6);
        content.set_margin_bottom(6);
        let add = gtk::Button::with_label("Add test tone");
        add.set_tooltip_text(Some("Create a 440 Hz test signal routed to the master bus"));
        let output = sender.output_sender().clone();
        add.connect_clicked(move |_| output.emit(AudioOutput::AddTestTone));
        content.append(&add);
        let health = gtk::Label::new(Some("Starting audio…"));
        health.set_wrap(true);
        health.add_css_class("caption");
        content.append(&health);
        let empty = gtk::Label::new(Some(
            "Add a test tone to measure audio. Device audio capture is coming next.",
        ));
        empty.set_wrap(true);
        empty.add_css_class("dim-label");
        content.append(&empty);
        let cards = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_vexpand(true);
        scroll.set_child(Some(&cards));
        content.append(&scroll);
        root.set_child(Some(&content));
        ComponentParts {
            model: Self {
                cards,
                empty,
                health,
                rows: BTreeMap::new(),
            },
            widgets: (),
        }
    }
    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>) {
        match message {
            AudioInput::Refresh(wake) => self.refresh(&wake.read(), &sender),
            AudioInput::Meters(wake) => self.meters(&wake.read()),
            AudioInput::StatusWake(wake) => self.update(AudioInput::Status(wake.read()), sender),
            AudioInput::Status(status) => self.health.set_label(match &status {
                prismcast_preview::AudioStatus::Starting => "Starting audio…",
                prismcast_preview::AudioStatus::Running => "Audio ready · monitoring off",
                prismcast_preview::AudioStatus::Failed(message) => message,
                prismcast_preview::AudioStatus::Stopped => "Audio stopped",
            }),
        }
    }
}
impl AudioPanel {
    fn refresh(&mut self, snapshot: &AppSnapshot, sender: &ComponentSender<Self>) {
        let state = snapshot.state();
        let visible = |source: &prismcast_core::Source| {
            (source.kind == SourceKind::TestPattern
                && source
                    .settings
                    .get("audio_test")
                    .and_then(|value| value.as_bool())
                    == Some(true))
                || matches!(
                    source.kind,
                    SourceKind::PipeWireAudioInput | SourceKind::PipeWireAppAudio
                )
                || state
                    .audio
                    .routes
                    .iter()
                    .any(|route| route.source_id == source.id)
        };
        self.rows.retain(|id, row| {
            let keep = state.sources.get(id).is_some_and(visible);
            if !keep {
                self.cards.remove(&row.widget);
            }
            keep
        });
        for source in state.sources.values().filter(|source| visible(source)) {
            let row = self.rows.entry(source.id).or_insert_with(|| {
                let row = AudioRow::new(source.id, sender);
                self.cards.append(&row.widget);
                row
            });
            let mixer = state.audio.mixer_state(source.id);
            row.restoring.set(true);
            row.name.set_label(&source.name);
            row.name.set_tooltip_text(Some(&source.name));
            // Changing unrelated state does not disturb an active slider.
            if !row.dragging.get() && (row.gain.value() - f64::from(mixer.volume_db)).abs() > 0.01 {
                row.gain.set_value(f64::from(mixer.volume_db));
            }
            row.muted.set_active(mixer.muted);
            row.solo.set_active(mixer.solo);
            row.gain.set_sensitive(source.enabled);
            row.muted.set_sensitive(source.enabled);
            row.solo.set_sensitive(source.enabled);
            let buses: Vec<_> = state
                .audio
                .routes
                .iter()
                .filter(|route| route.source_id == source.id)
                .filter_map(|route| {
                    state
                        .audio
                        .buses
                        .iter()
                        .find(|bus| bus.id == route.bus_id)
                        .map(|bus| bus.name.as_str())
                })
                .collect();
            row.route.set_label(&if !source.enabled {
                "Disabled".into()
            } else if buses.is_empty() {
                "Unrouted".into()
            } else {
                buses.join(", ")
            });
            let is_tone = source.kind == SourceKind::TestPattern
                && source
                    .settings
                    .get("audio_test")
                    .and_then(|value| value.as_bool())
                    == Some(true);
            row.remove.set_visible(is_tone);
            let mut commands: Vec<_> = state
                .audio
                .routes
                .iter()
                .filter(|route| route.source_id == source.id)
                .map(|route| Command::RemoveAudioRoute {
                    source_id: source.id,
                    bus_id: route.bus_id,
                })
                .collect();
            commands.push(Command::RemoveSource {
                source_id: source.id,
            });
            *row.removal.borrow_mut() = Some(Command::Transaction { commands });
            row.restoring.set(false);
            row.reading.set_label("No audio data");
            row.level.set_value(0.0);
        }
        self.empty.set_visible(self.rows.is_empty());
    }
    fn meters(&self, snapshot: &MeterSnapshot) {
        // No widgets are created/replaced by the high-rate observation path.
        for (id, row) in &self.rows {
            if let Some(levels) = snapshot.levels.get(id) {
                let peak = levels.peak_dbfs.iter().copied().fold(-120.0_f32, f32::max);
                let rms = levels.rms_dbfs.iter().copied().fold(-120.0_f32, f32::max);
                row.level
                    .set_value(f64::from(((peak + 60.0) / 60.0).clamp(0.0, 1.0)));
                row.reading
                    .set_label(&format!("Peak {peak:.1} · RMS {rms:.1} dBFS"));
            } else {
                row.level.set_value(0.0);
                row.reading.set_label("No audio data");
            }
        }
    }
}
impl AudioRow {
    fn new(source_id: SourceId, sender: &ComponentSender<AudioPanel>) -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 3);
        let name = gtk::Label::new(None);
        name.set_xalign(0.0);
        name.add_css_class("heading");
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        name.set_max_width_chars(28);
        widget.append(&name);
        let route = gtk::Label::new(None);
        route.set_xalign(0.0);
        route.add_css_class("caption");
        widget.append(&route);
        let controls = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let gain = gtk::Scale::with_range(gtk::Orientation::Horizontal, -60.0, 20.0, 0.5);
        gain.set_value(0.0);
        gain.set_hexpand(true);
        gain.set_tooltip_text(Some("Gain in decibels"));
        let muted = gtk::CheckButton::with_label("Mute");
        let solo = gtk::CheckButton::with_label("Solo");
        controls.append(&gain);
        controls.append(&muted);
        controls.append(&solo);
        widget.append(&controls);
        let restoring = Rc::new(Cell::new(false));
        let dragging = Rc::new(Cell::new(false));
        let pointer = gtk::GestureClick::new();
        pointer.set_propagation_phase(gtk::PropagationPhase::Capture);
        pointer.connect_pressed({
            let dragging = dragging.clone();
            move |_, _, _, _| dragging.set(true)
        });
        pointer.connect_released({
            let dragging = dragging.clone();
            let gain = gain.downgrade();
            let output = sender.output_sender().clone();
            move |_, _, _, _| {
                if dragging.replace(false) {
                    if let Some(gain) = gain.upgrade() {
                        output.emit(AudioOutput::Command(Box::new(Command::SetSourceVolume {
                            source_id,
                            volume_db: gain.value() as f32,
                        })));
                    }
                }
            }
        });
        pointer.connect_cancel({
            let dragging = dragging.clone();
            move |_, _| dragging.set(false)
        });
        gain.add_controller(pointer);
        gain.connect_value_changed({
            let restoring = restoring.clone();
            let dragging = dragging.clone();
            let output = sender.output_sender().clone();
            move |gain| {
                if !restoring.get() && !dragging.get() {
                    output.emit(AudioOutput::Command(Box::new(Command::SetSourceVolume {
                        source_id,
                        volume_db: gain.value() as f32,
                    })));
                }
            }
        });
        muted.connect_toggled({
            let restoring = restoring.clone();
            let output = sender.output_sender().clone();
            move |button| {
                if !restoring.get() {
                    output.emit(AudioOutput::Command(Box::new(Command::SetSourceMuted {
                        source_id,
                        muted: button.is_active(),
                    })));
                }
            }
        });
        solo.connect_toggled({
            let restoring = restoring.clone();
            let output = sender.output_sender().clone();
            move |button| {
                if !restoring.get() {
                    output.emit(AudioOutput::Command(Box::new(Command::SetSourceSolo {
                        source_id,
                        solo: button.is_active(),
                    })));
                }
            }
        });
        let level = gtk::LevelBar::for_interval(0.0, 1.0);
        widget.append(&level);
        let reading = gtk::Label::new(Some("No audio data"));
        reading.set_xalign(0.0);
        reading.add_css_class("caption");
        widget.append(&reading);
        let remove = gtk::Button::with_label("Remove test tone");
        remove.add_css_class("flat");
        let removal = Rc::new(RefCell::new(None::<Command>));
        remove.connect_clicked({
            let removal = removal.clone();
            let output = sender.output_sender().clone();
            move |_| {
                if let Some(command) = removal.borrow().clone() {
                    output.emit(AudioOutput::Command(Box::new(command)));
                }
            }
        });
        widget.append(&remove);
        Self {
            widget,
            name,
            route,
            gain,
            muted,
            solo,
            level,
            reading,
            restoring,
            dragging,
            remove,
            removal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use relm4::{Component, ComponentController};

    fn drain() {
        let context = gtk::glib::MainContext::default();
        for _ in 0..500 {
            if !context.pending() {
                break;
            }
            context.iteration(false);
        }
    }

    #[test]
    #[ignore = "requires a real GTK display; run separately with --ignored --test-threads=1"]
    fn audio_controls_send_commands_and_meter_updates_preserve_widgets() {
        adw::init().unwrap();
        let (bridge, thread) = crate::bridge::CoreBridge::spawn_background().unwrap();
        let handle = bridge.handle().clone();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let source_id = runtime.block_on(async {
            let response = handle
                .dispatch(Command::AddSource {
                    kind: SourceKind::TestPattern,
                    name: "Meter signal".into(),
                })
                .await
                .unwrap();
            let id = response
                .events
                .iter()
                .find_map(|event| match event {
                    prismcast_core::Event::Source(prismcast_core::SourceEvent::Added {
                        source,
                    }) => Some(source.id),
                    _ => None,
                })
                .unwrap();
            handle
                .dispatch(Command::SetSourceSettings {
                    source_id: id,
                    settings: serde_json::json!({"audio_test":true}),
                })
                .await
                .unwrap();
            handle
                .dispatch(Command::SetSourceVolume {
                    source_id: id,
                    volume_db: -6.0,
                })
                .await
                .unwrap();
            handle
                .dispatch(Command::AddAudioBus {
                    name: "Second mix".into(),
                })
                .await
                .unwrap();
            let buses = handle.snapshot().state().audio.buses.clone();
            for bus in buses {
                handle
                    .dispatch(Command::SetAudioRoute {
                        source_id: id,
                        bus_id: bus.id,
                        tracks: prismcast_core::audio::TrackMask::stereo_pair(),
                    })
                    .await
                    .unwrap();
            }
            id
        });
        let observed = Rc::new(RefCell::new(Vec::new()));
        let capture = observed.clone();
        let panel = AudioPanel::builder()
            .launch(())
            .connect_receiver(move |_, message| capture.borrow_mut().push(message));
        let window = adw::Window::new();
        window.set_default_size(400, 300);
        window.set_content(Some(panel.widget()));
        window.present();
        panel.emit(AudioInput::Refresh(SnapshotRefresh::new(handle.clone())));
        drain();
        assert!(
            observed.borrow().is_empty(),
            "rendering must never dispatch commands"
        );
        let widget = panel.model().rows[&source_id].widget.clone();
        let gain = panel.model().rows[&source_id].gain.clone();
        let mute = panel.model().rows[&source_id].muted.clone();
        let solo = panel.model().rows[&source_id].solo.clone();
        assert_eq!(gain.value(), -6.0);
        gain.set_value(-12.0);
        mute.set_active(true);
        solo.set_active(true);
        drain();
        let messages = observed.borrow();
        assert_eq!(messages.len(), 3);
        assert!(
            matches!(&messages[0], AudioOutput::Command(command) if matches!(command.as_ref(), Command::SetSourceVolume { source_id: id, volume_db } if *id == source_id && *volume_db == -12.0))
        );
        assert!(
            matches!(&messages[1], AudioOutput::Command(command) if matches!(command.as_ref(), Command::SetSourceMuted { muted: true, .. }))
        );
        assert!(
            matches!(&messages[2], AudioOutput::Command(command) if matches!(command.as_ref(), Command::SetSourceSolo { solo: true, .. }))
        );
        drop(messages);
        let meter = MeterSnapshot {
            levels: BTreeMap::from([(
                source_id,
                prismcast_app::SourceMeter {
                    source_id,
                    peak_dbfs: vec![-7.0, -8.0],
                    rms_dbfs: vec![-10.0, -11.0],
                },
            )]),
        };
        panel.model().meters(&meter);
        assert_eq!(panel.model().rows[&source_id].widget, widget);
        assert_eq!(
            gain.value(),
            -12.0,
            "meter refresh must not interrupt editing"
        );
        assert!(panel.model().rows[&source_id]
            .reading
            .text()
            .contains("Peak -7.0"));
        assert_eq!(observed.borrow().len(), 3);
        panel.model().meters(&MeterSnapshot::default());
        assert_eq!(
            panel.model().rows[&source_id].reading.text(),
            "No audio data"
        );
        panel.model().rows[&source_id].remove.emit_clicked();
        drain();
        let deletion = match observed.borrow().last().unwrap() {
            AudioOutput::Command(command) => command.as_ref().clone(),
            _ => panic!("expected deletion command"),
        };
        assert!(
            matches!(&deletion, Command::Transaction { commands } if commands.len() == 3 && matches!(commands.last(), Some(Command::RemoveSource { source_id: id }) if *id == source_id))
        );
        runtime.block_on(handle.dispatch(deletion)).unwrap();
        assert!(!handle.snapshot().state().sources.contains_key(&source_id));
        assert!(handle.snapshot().state().audio.routes.is_empty());
        panel.emit(AudioInput::Refresh(SnapshotRefresh::new(handle.clone())));
        drain();
        assert!(panel.model().rows.is_empty());
        window.close();
        drop(panel);
        drain();
        runtime.block_on(handle.shutdown());
        thread.join().unwrap();
    }
}
