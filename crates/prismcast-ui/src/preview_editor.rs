//! GTK-local preview selection/drafts. Only completed intents leave as Commands.
use gtk::prelude::*;
use prismcast_app::AppSnapshot;
use prismcast_compositor::{anchor_fractions, layout_item, ItemLayout, SourceSize};
use prismcast_core::{
    BoundsKind, Command, SceneId, SceneItem, SceneItemId, Source, Transform, VideoConfig,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
};

#[derive(Debug, Clone, Copy, PartialEq)]
struct Mapping {
    x: f64,
    y: f64,
    scale: f64,
    width: f64,
    height: f64,
}
impl Mapping {
    fn new(width: f64, height: f64, video: VideoConfig) -> Option<Self> {
        if !width.is_finite()
            || !height.is_finite()
            || width <= 0.0
            || height <= 0.0
            || video.width == 0
            || video.height == 0
        {
            return None;
        }
        let scale = (width / video.width as f64).min(height / video.height as f64);
        let w = video.width as f64 * scale;
        let h = video.height as f64 * scale;
        Some(Self {
            x: (width - w) / 2.0,
            y: (height - h) / 2.0,
            scale,
            width: w,
            height: h,
        })
    }
    fn canvas(self, x: f64, y: f64) -> Option<(f64, f64)> {
        if !x.is_finite()
            || !y.is_finite()
            || x < self.x
            || y < self.y
            || x >= self.x + self.width
            || y >= self.y + self.height
        {
            return None;
        }
        Some(((x - self.x) / self.scale, (y - self.y) / self.scale))
    }
}
fn video(snapshot: &AppSnapshot) -> VideoConfig {
    snapshot
        .state()
        .active_profile
        .and_then(|id| snapshot.state().profiles.get(&id))
        .map(|profile| profile.video)
        .unwrap_or(VideoConfig {
            width: 1280,
            height: 720,
            fps_num: 30,
            fps_den: 1,
        })
}
fn rotate90(rotation: f32) -> f32 {
    ((rotation.rem_euclid(360.0) / 90.0).round() * 90.0 + 90.0).rem_euclid(360.0)
}
fn source_size(snapshot: Option<&AppSnapshot>, source: &Source) -> Option<SourceSize> {
    let negotiated = snapshot
        .and_then(|snapshot| snapshot.source_runtime(source.id))
        .filter(|runtime| runtime.status == prismcast_core::CaptureStatus::Active)
        .and_then(|runtime| runtime.dimensions)
        .map(|dimensions| SourceSize {
            width: dimensions.width,
            height: dimensions.height,
        });
    prismcast_compositor::source_size(source, negotiated).ok()
}
fn geometry(
    item: &SceneItem,
    source: &Source,
    snapshot: Option<&AppSnapshot>,
) -> Option<ItemLayout> {
    if !item.visible || item.opacity <= 0.0 || !source.enabled {
        return None;
    }
    layout_item(item, source_size(snapshot, source)?).ok()
}
#[derive(Clone)]
struct Draft {
    scene_id: SceneId,
    item: SceneItem,
    source: Source,
    runtime: Option<prismcast_core::SourceRuntime>,
    mapping: Mapping,
    layout: ItemLayout,
    resize: bool,
    transform: Transform,
}
impl Draft {
    fn update(&mut self, dx: f64, dy: f64) {
        if !dx.is_finite() || !dy.is_finite() {
            return;
        }
        let dx = (dx / self.mapping.scale) as f32;
        let dy = (dy / self.mapping.scale) as f32;
        self.transform = self.item.transform;
        if self.resize {
            let width = (self.layout.rect.width as f32 + dx).clamp(
                1.0,
                (64.0 * self.layout.rotated_source_size.width as f32).min(8192.0),
            );
            let height = (self.layout.rect.height as f32 + dy).clamp(
                1.0,
                (64.0 * self.layout.rotated_source_size.height as f32).min(8192.0),
            );
            let (ax, ay) = anchor_fractions(self.item.transform.anchor);
            self.transform.scale.x = if self.item.transform.scale.x < 0.0 {
                -1.0
            } else {
                1.0
            } * width
                / self.layout.rotated_source_size.width as f32;
            self.transform.scale.y = if self.item.transform.scale.y < 0.0 {
                -1.0
            } else {
                1.0
            } * height
                / self.layout.rotated_source_size.height as f32;
            self.transform.scale.x = self.transform.scale.x.clamp(-64.0, 64.0);
            self.transform.scale.y = self.transform.scale.y.clamp(-64.0, 64.0);
            self.transform.position.x = self.layout.rect.x as f32 + ax * width;
            self.transform.position.y = self.layout.rect.y as f32 + ay * height;
        } else {
            self.transform.position.x =
                (self.transform.position.x + dx).clamp(-1_000_000.0, 1_000_000.0);
            self.transform.position.y =
                (self.transform.position.y + dy).clamp(-1_000_000.0, 1_000_000.0);
        }
    }
}
#[derive(Default)]
struct State {
    snapshot: Option<Arc<AppSnapshot>>,
    selected: Option<SceneItemId>,
    order: Vec<SceneItemId>,
    labels: Vec<String>,
    draft: Option<Draft>,
    available: bool,
    pending: bool,
}
impl State {
    fn selected(&self) -> Option<(SceneId, SceneItem, Source, ItemLayout)> {
        let snapshot = self.snapshot.as_ref()?;
        let scene = snapshot.scene(snapshot.current_scene()?)?;
        let item = scene.item(self.selected?)?.clone();
        let source = snapshot.source(item.source_id)?.clone();
        let layout = geometry(&item, &source, Some(snapshot))?;
        Some((scene.id, item, source, layout))
    }
    fn refresh(&mut self, snapshot: Arc<AppSnapshot>) {
        let context_changed = self.snapshot.as_ref().is_some_and(|old| {
            old.current_scene() != snapshot.current_scene()
                || old.state().active_profile != snapshot.state().active_profile
                || video(old) != video(&snapshot)
        });
        if context_changed {
            self.selected = None;
            self.draft = None;
        }
        if let Some(draft) = &self.draft {
            let unchanged = snapshot.current_scene() == Some(draft.scene_id)
                && snapshot
                    .scene(draft.scene_id)
                    .and_then(|scene| scene.item(draft.item.id))
                    == Some(&draft.item)
                && snapshot.source(draft.source.id) == Some(&draft.source)
                && snapshot.source_runtime(draft.source.id) == draft.runtime.as_ref();
            if !unchanged {
                self.draft = None;
            }
        }
        self.snapshot = Some(snapshot);
        if self.selected().is_none() {
            self.selected = None;
            self.draft = None;
        }
    }
    fn begin(&mut self, x: f64, y: f64, width: f64, height: f64) {
        self.draft = None;
        if !self.available || self.pending {
            return;
        }
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let Some(mapping) = Mapping::new(width, height, video(snapshot)) else {
            return;
        };
        let Some((cx, cy)) = mapping.canvas(x, y) else {
            return;
        };
        let Some(scene) = snapshot.current_scene().and_then(|id| snapshot.scene(id)) else {
            return;
        };
        let hit = scene.items.iter().rev().find_map(|item| {
            let source = snapshot.source(item.source_id)?;
            let layout = geometry(item, source, Some(snapshot))?;
            layout
                .rect
                .contains(cx, cy)
                .then_some((item.clone(), source.clone(), layout))
        });
        self.selected = hit.as_ref().map(|(item, _, _)| item.id);
        if let Some((item, source, layout)) = hit {
            if item.locked {
                return;
            }
            let resize = item.bounds.kind == BoundsKind::None
                && ((cx - layout.rect.x as f64 - layout.rect.width as f64).abs() * mapping.scale
                    <= 12.0)
                && ((cy - layout.rect.y as f64 - layout.rect.height as f64).abs() * mapping.scale
                    <= 12.0);
            let transform = item.transform;
            self.draft = Some(Draft {
                scene_id: scene.id,
                item,
                runtime: snapshot.source_runtime(source.id).cloned(),
                source,
                mapping,
                layout,
                resize,
                transform,
            });
        }
    }
    fn finish(&mut self, dx: f64, dy: f64) -> Option<Command> {
        let mut draft = self.draft.take()?;
        if !self.available || self.pending {
            return None;
        }
        if dx == 0.0 && dy == 0.0 {
            return None;
        }
        draft.update(dx, dy);
        if draft.transform == draft.item.transform {
            return None;
        }
        let mut candidate = draft.item.clone();
        candidate.transform = draft.transform;
        geometry(&candidate, &draft.source, self.snapshot.as_deref())?;
        self.pending = true;
        Some(Command::SetSceneItemTransform {
            scene_id: draft.scene_id,
            item_id: draft.item.id,
            transform: draft.transform,
        })
    }
}
struct Inner {
    widget: gtk::Box,
    area: gtk::DrawingArea,
    selector: gtk::DropDown,
    controls: gtk::Box,
    fields: [gtk::SpinButton; 4],
    hint: gtk::Label,
    gesture: gtk::GestureDrag,
    state: RefCell<State>,
    syncing: Cell<bool>,
    send: Box<dyn Fn(Command)>,
    latest: Box<dyn Fn() -> Arc<AppSnapshot>>,
}
/// The root owns this local editor; callbacks retain only weak references.
pub struct PreviewEditor(Rc<Inner>);
impl PreviewEditor {
    pub fn new(
        picture: &gtk::Picture,
        latest: impl Fn() -> Arc<AppSnapshot> + 'static,
        send: impl Fn(Command) + 'static,
    ) -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let overlay = gtk::Overlay::new();
        overlay.set_vexpand(true);
        overlay.set_child(Some(picture));
        let area = gtk::DrawingArea::new();
        area.set_hexpand(true);
        area.set_vexpand(true);
        area.set_focusable(true);
        area.add_css_class("accent");
        area.set_tooltip_text(Some(
            "Select and drag a placement; drag its lower-right handle to resize. Escape cancels.",
        ));
        overlay.add_overlay(&area);
        widget.append(&overlay);
        let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let selector = gtk::DropDown::from_strings(&["No placement"]);
        selector.set_hexpand(true);
        toolbar.append(&gtk::Label::new(Some("Placement:")));
        toolbar.append(&selector);
        widget.append(&toolbar);
        let controls = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let fields = std::array::from_fn(|i| {
            gtk::SpinButton::with_range(
                if i < 2 { -1_000_000.0 } else { -64.0 },
                if i < 2 { 1_000_000.0 } else { 64.0 },
                if i < 2 { 1.0 } else { 0.05 },
            )
        });
        for (label, field) in ["X", "Y", "Scale X", "Scale Y"].iter().zip(&fields) {
            field.set_digits(2);
            field.set_width_chars(7);
            let label = gtk::Label::new(Some(label));
            label.set_mnemonic_widget(Some(field));
            controls.append(&label);
            controls.append(field);
        }
        let buttons = [
            gtk::Button::with_label("Apply"),
            gtk::Button::with_label("Rotate 90°"),
            gtk::Button::with_label("Flip X"),
            gtk::Button::with_label("Flip Y"),
        ];
        for button in &buttons {
            controls.append(button);
        }
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Never);
        scroll.set_child(Some(&controls));
        widget.append(&scroll);
        let hint = gtk::Label::new(Some("Select a visible placement to edit."));
        hint.set_wrap(true);
        hint.set_height_request(48);
        hint.set_xalign(0.0);
        widget.append(&hint);
        let gesture = gtk::GestureDrag::new();
        gesture.set_button(1);
        area.add_controller(gesture.clone());
        let inner = Rc::new(Inner {
            widget,
            area,
            selector,
            controls,
            fields,
            hint,
            gesture,
            state: RefCell::new(State::default()),
            syncing: Cell::new(false),
            send: Box::new(send),
            latest: Box::new(latest),
        });
        inner.area.set_draw_func({
            let weak = Rc::downgrade(&inner);
            move |_, context, width, height| {
                if let Some(inner) = weak.upgrade() {
                    inner.draw(context, width, height);
                }
            }
        });
        inner.gesture.connect_drag_begin({
            let weak = Rc::downgrade(&inner);
            move |_, x, y| {
                if let Some(inner) = weak.upgrade() {
                    inner.state.borrow_mut().refresh((inner.latest)());
                    inner.state.borrow_mut().begin(
                        x,
                        y,
                        inner.area.width() as f64,
                        inner.area.height() as f64,
                    );
                    inner.sync();
                    inner.area.grab_focus();
                }
            }
        });
        inner.gesture.connect_drag_update({
            let weak = Rc::downgrade(&inner);
            move |_, dx, dy| {
                if let Some(inner) = weak.upgrade() {
                    if let Some(draft) = inner.state.borrow_mut().draft.as_mut() {
                        draft.update(dx, dy);
                    }
                    inner.area.queue_draw();
                }
            }
        });
        inner.gesture.connect_drag_end({
            let weak = Rc::downgrade(&inner);
            move |_, dx, dy| {
                if let Some(inner) = weak.upgrade() {
                    inner.state.borrow_mut().refresh((inner.latest)());
                    let command = inner.state.borrow_mut().finish(dx, dy);
                    if let Some(command) = command {
                        (inner.send)(command);
                    }
                    inner.sync();
                }
            }
        });
        inner.gesture.connect_cancel({
            let weak = Rc::downgrade(&inner);
            move |_, _| {
                if let Some(inner) = weak.upgrade() {
                    inner.state.borrow_mut().draft = None;
                    inner.sync();
                }
            }
        });
        inner.area.connect_resize({
            let weak = Rc::downgrade(&inner);
            move |_, _, _| {
                if let Some(inner) = weak.upgrade() {
                    inner.state.borrow_mut().draft = None;
                    inner.area.queue_draw();
                }
            }
        });
        let key = gtk::EventControllerKey::new();
        key.connect_key_pressed({
            let weak = Rc::downgrade(&inner);
            move |_, key, _, _| {
                if key == gtk::gdk::Key::Escape {
                    if let Some(inner) = weak.upgrade() {
                        inner.state.borrow_mut().draft = None;
                        inner.sync();
                    }
                    gtk::glib::Propagation::Stop
                } else {
                    gtk::glib::Propagation::Proceed
                }
            }
        });
        inner.area.add_controller(key);
        inner.selector.connect_selected_notify({
            let weak = Rc::downgrade(&inner);
            move |selector| {
                if let Some(inner) = weak.upgrade() {
                    if inner.syncing.get() {
                        return;
                    }
                    let id = selector
                        .selected()
                        .checked_sub(1)
                        .and_then(|index| inner.state.borrow().order.get(index as usize).copied());
                    if inner.state.borrow().selected == id {
                        return;
                    }
                    inner.state.borrow_mut().selected = id;
                    inner.state.borrow_mut().draft = None;
                    inner.sync();
                }
            }
        });
        for (action, button) in buttons.into_iter().enumerate() {
            button.connect_clicked({
                let weak = Rc::downgrade(&inner);
                move |_| {
                    if let Some(inner) = weak.upgrade() {
                        inner.action(action);
                    }
                }
            });
        }
        Self(inner)
    }
    pub fn widget(&self) -> &gtk::Box {
        &self.0.widget
    }
    pub fn refresh(&self, snapshot: Arc<AppSnapshot>) {
        self.0.state.borrow_mut().refresh(snapshot);
        self.0.sync();
    }
    pub fn set_available(&self, available: bool) {
        let mut state = self.0.state.borrow_mut();
        state.available = available;
        if !available {
            state.draft = None;
            state.selected = None;
        }
        drop(state);
        self.0.sync();
    }
    pub fn completed(&self, snapshot: Arc<AppSnapshot>) {
        self.0.state.borrow_mut().pending = false;
        self.refresh(snapshot);
    }
}
impl Inner {
    fn sync(&self) {
        self.syncing.set(true);
        let mut state = self.state.borrow_mut();
        let choices = state
            .snapshot
            .as_ref()
            .and_then(|s| {
                s.current_scene().and_then(|id| s.scene(id)).map(|scene| {
                    scene
                        .items
                        .iter()
                        .filter_map(|item| {
                            let source = s.source(item.source_id)?;
                            geometry(item, source, Some(s))?;
                            Some((
                                item.id,
                                format!(
                                    "{} — placement {}{}",
                                    source.name,
                                    scene
                                        .items
                                        .iter()
                                        .position(|candidate| candidate.id == item.id)
                                        .map(|index| index + 1)
                                        .unwrap_or(1),
                                    if item.locked { " (locked)" } else { "" }
                                ),
                            ))
                        })
                        .collect::<Vec<_>>()
                })
            })
            .unwrap_or_default();
        let order = choices.iter().map(|(id, _)| *id).collect::<Vec<_>>();
        let names = choices
            .iter()
            .map(|(_, name)| name.clone())
            .collect::<Vec<_>>();
        if state.order != order || state.labels != names {
            state.order = order;
            state.labels = names;
            let mut labels = vec!["Select a placement…"];
            labels.extend(state.labels.iter().map(String::as_str));
            self.selector
                .set_model(Some(&gtk::StringList::new(&labels)));
        }
        self.selector.set_selected(
            state
                .selected
                .and_then(|id| state.order.iter().position(|candidate| *candidate == id))
                .map(|index| index as u32 + 1)
                .unwrap_or(0),
        );
        let selected = state.selected();
        self.controls.set_sensitive(
            state.available
                && !state.pending
                && selected
                    .as_ref()
                    .is_some_and(|(_, item, _, _)| !item.locked),
        );
        self.selector
            .set_sensitive(state.available && !state.pending && !choices.is_empty());
        if let Some((_, item, _, _)) = selected {
            for (field, value) in self.fields.iter().zip([
                item.transform.position.x,
                item.transform.position.y,
                item.transform.scale.x,
                item.transform.scale.y,
            ]) {
                field.set_value(value as f64);
            }
            self.fields[2].set_sensitive(item.bounds.kind == BoundsKind::None);
            self.fields[3].set_sensitive(item.bounds.kind == BoundsKind::None);
            self.hint.set_label(if item.locked{"Placement is locked. Unlock it in Sources to edit."}else if item.bounds.kind!=BoundsKind::None{"Bounds control sizing; pointer resize and scale edits are disabled."}else{"Drag to move; drag the lower-right handle to resize. Escape cancels. Apply commits numeric edits."});
        } else {
            self.hint.set_label(if state.available {
                "Select a visible placement to edit."
            } else {
                "Editing is available when the preview is running."
            });
        }
        self.syncing.set(false);
        drop(state);
        self.area.queue_draw();
    }
    fn action(&self, action: usize) {
        let mut state = self.state.borrow_mut();
        let expected = state.selected();
        let expected_runtime = expected.as_ref().and_then(|(_, _, source, _)| {
            state.snapshot.as_ref()?.source_runtime(source.id).cloned()
        });
        let previous_video = state.snapshot.as_ref().map(|snapshot| video(snapshot));
        let latest = (self.latest)();
        let changed = previous_video != Some(video(&latest));
        state.refresh(latest);
        let current_runtime = state.selected().as_ref().and_then(|(_, _, source, _)| {
            state.snapshot.as_ref()?.source_runtime(source.id).cloned()
        });
        if changed
            || expected_runtime != current_runtime
            || expected
                .as_ref()
                .map(|(scene, item, source, layout)| (*scene, item, source, *layout))
                != state
                    .selected()
                    .as_ref()
                    .map(|(scene, item, source, layout)| (*scene, item, source, *layout))
        {
            drop(state);
            self.sync();
            self.hint.set_label(
                "Placement changed elsewhere; review the current values before applying.",
            );
            return;
        }
        if !state.available || state.pending || state.draft.is_some() {
            return;
        }
        let Some((scene_id, item, source, _)) = state.selected() else {
            return;
        };
        if item.locked {
            return;
        }
        let mut transform = item.transform;
        match action {
            0 => {
                transform.position.x = self.fields[0].value() as f32;
                transform.position.y = self.fields[1].value() as f32;
                if item.bounds.kind == BoundsKind::None {
                    transform.scale.x = self.fields[2].value() as f32;
                    transform.scale.y = self.fields[3].value() as f32;
                }
            }
            1 => transform.rotation = rotate90(transform.rotation),
            2 => transform.scale.x = -transform.scale.x,
            3 => transform.scale.y = -transform.scale.y,
            _ => return,
        }
        if transform == item.transform {
            return;
        }
        let mut candidate = item.clone();
        candidate.transform = transform;
        if let Err(error) = source_size(state.snapshot.as_deref(), &source)
            .ok_or_else(|| {
                prismcast_core::Error::InvalidInput("Capture dimensions are unavailable".into())
            })
            .and_then(|size| layout_item(&candidate, size))
        {
            self.hint
                .set_label(&format!("Cannot apply this transform: {error}"));
            return;
        }
        state.pending = true;
        drop(state);
        (self.send)(Command::SetSceneItemTransform {
            scene_id,
            item_id: item.id,
            transform,
        });
        self.sync();
    }
    fn draw(&self, context: &gtk::cairo::Context, width: i32, height: i32) {
        let state = self.state.borrow();
        if !state.available {
            return;
        }
        let Some(snapshot) = &state.snapshot else {
            return;
        };
        let Some(mapping) = Mapping::new(width as f64, height as f64, video(snapshot)) else {
            return;
        };
        let Some((_, mut item, source, _)) = state.selected() else {
            return;
        };
        if let Some(draft) = &state.draft {
            item.transform = draft.transform;
        }
        let Some(layout) = geometry(&item, &source, Some(snapshot)) else {
            return;
        };
        let rect = layout.rect;
        if context.save().is_err() {
            return;
        }
        context.rectangle(mapping.x, mapping.y, mapping.width, mapping.height);
        context.clip();
        let color = self.area.color();
        context.set_source_rgba(
            color.red() as f64,
            color.green() as f64,
            color.blue() as f64,
            color.alpha() as f64,
        );
        if item.locked {
            context.set_dash(&[4.0, 4.0], 0.0);
        }
        context.set_line_width(2.0);
        let x = mapping.x + rect.x as f64 * mapping.scale;
        let y = mapping.y + rect.y as f64 * mapping.scale;
        let w = rect.width as f64 * mapping.scale;
        let h = rect.height as f64 * mapping.scale;
        context.rectangle(x, y, w, h);
        let _ = context.stroke();
        if !item.locked && item.bounds.kind == BoundsKind::None {
            context.rectangle(x + w - 6.0, y + h - 6.0, 12.0, 12.0);
            let _ = context.fill();
        }
        let _ = context.restore();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn capture_geometry_requires_native_caps_and_runtime_changes_cancel_drafts() {
        use prismcast_core::{CaptureStatus, SourceDimensions, SourceKind};
        let handle = prismcast_app::AppHandle::spawn(prismcast_app::CoreConfig::default());
        let mut owner = handle.attach_capture_owner().await.unwrap();
        handle
            .dispatch(Command::AddScene {
                name: "Capture".into(),
            })
            .await
            .unwrap();
        handle
            .dispatch(Command::AddSource {
                kind: SourceKind::PipeWireWindow,
                name: "Window".into(),
            })
            .await
            .unwrap();
        let source_id = handle.snapshot().sources().next().unwrap().id;
        let scene_id = handle.snapshot().current_scene().unwrap();
        handle
            .dispatch(Command::AddSceneItem {
                scene_id,
                source_id,
            })
            .await
            .unwrap();
        assert!(
            owner.requests.try_recv().is_err(),
            "creation must never open a picker"
        );
        let mut state = State {
            available: true,
            ..State::default()
        };
        state.refresh(handle.snapshot());
        state.begin(20.0, 20.0, 1280.0, 720.0);
        assert!(state.draft.is_none(), "capture has no invented dimensions");
        handle
            .authorize_source_capture(source_id, Some("wayland:test-parent".into()))
            .await
            .unwrap();
        let request = owner.requests.recv().await.unwrap();
        assert_eq!(
            request.parent_window.as_deref(),
            Some("wayland:test-parent")
        );
        owner
            .runtime
            .report(
                source_id,
                request.generation,
                CaptureStatus::Active,
                Some(SourceDimensions {
                    width: 100,
                    height: 50,
                }),
                None,
            )
            .await
            .unwrap();
        state.refresh(handle.snapshot());
        state.begin(20.0, 20.0, 1280.0, 720.0);
        let draft = state.draft.as_ref().unwrap();
        assert_eq!(
            (draft.layout.rect.width, draft.layout.rect.height),
            (100, 50)
        );
        owner
            .runtime
            .report(
                source_id,
                request.generation,
                CaptureStatus::Active,
                Some(SourceDimensions {
                    width: 200,
                    height: 100,
                }),
                None,
            )
            .await
            .unwrap();
        state.refresh(handle.snapshot());
        assert!(
            state.draft.is_none(),
            "renegotiated caps invalidate captured gesture geometry"
        );
        state.begin(20.0, 20.0, 1280.0, 720.0);
        assert_eq!(state.draft.as_ref().unwrap().layout.rect.width, 200);
        owner
            .runtime
            .report(
                source_id,
                request.generation,
                CaptureStatus::Revoked,
                None,
                Some("Permission revoked".into()),
            )
            .await
            .unwrap();
        state.refresh(handle.snapshot());
        assert!(state.finish(10.0, 10.0).is_none());
        assert!(state.selected.is_none());
        handle.shutdown().await;
    }
    #[test]
    fn letterbox_mapping_rejects_margins_and_maps_canvas() {
        assert_eq!(rotate90(-45.0), 90.0);
        assert_eq!(rotate90(315.0), 90.0);
        let video = VideoConfig {
            width: 1920,
            height: 1080,
            fps_num: 60,
            fps_den: 1,
        };
        let map = Mapping::new(1000.0, 1000.0, video).unwrap();
        assert_eq!(map.canvas(500.0, 0.0), None);
        let (x, y) = map.canvas(500.0, 500.0).unwrap();
        assert!((x - 960.0).abs() < 0.01);
        assert!((y - 540.0).abs() < 0.01);
        assert!(Mapping::new(0.0, 10.0, video).is_none());
        assert_eq!(map.canvas(f64::NAN, 500.0), None);
    }
    fn draft(width: u32, height: u32, transform: Transform) -> Draft {
        let mut source = Source::new(prismcast_core::SourceKind::TestPattern, "fixture");
        source.settings = serde_json::json!({"width":width,"height":height});
        let mut item = SceneItem::new(source.id, 0);
        item.transform = transform;
        let layout = geometry(&item, &source, None).unwrap();
        Draft {
            scene_id: SceneId::new(),
            item,
            source,
            runtime: None,
            mapping: Mapping {
                x: 0.0,
                y: 0.0,
                scale: 1.0,
                width: 1000.0,
                height: 1000.0,
            },
            layout,
            resize: true,
            transform,
        }
    }
    #[test]
    fn resize_clamps_before_anchor_math_and_keeps_fixed_rendered_corner() {
        let mut draft = draft(
            1,
            1,
            Transform {
                anchor: prismcast_core::Anchor::Center,
                ..Transform::default()
            },
        );
        let original = draft.layout.rect;
        draft.update(8191.0, 8191.0);
        let mut item = draft.item.clone();
        item.transform = draft.transform;
        let final_rect = geometry(&item, &draft.source, None).unwrap().rect;
        assert_eq!((final_rect.x, final_rect.y), (original.x, original.y));
        assert_eq!((final_rect.width, final_rect.height), (64, 64));
    }
    #[test]
    fn zero_offset_and_cancel_leave_fractional_transform_untouched() {
        let draft = draft(
            10,
            20,
            Transform {
                scale: prismcast_core::Vec2::new(-0.125, 1.125),
                ..Transform::default()
            },
        );
        let mut state = State {
            available: true,
            draft: Some(draft.clone()),
            ..State::default()
        };
        assert!(state.finish(0.0, 0.0).is_none());
        assert!(!state.pending);
        state.draft = Some(draft);
        state.draft = None;
        assert!(state.finish(20.0, 30.0).is_none());
    }
    #[test]
    fn rotated_signed_resize_uses_shared_intrinsic_axes() {
        let mut draft = draft(
            10,
            20,
            Transform {
                rotation: 90.0,
                scale: prismcast_core::Vec2::new(-1.0, 1.0),
                anchor: prismcast_core::Anchor::Center,
                ..Transform::default()
            },
        );
        let original = draft.layout.rect;
        draft.update(20.0, 10.0);
        assert_eq!(draft.transform.scale, prismcast_core::Vec2::new(-2.0, 2.0));
        let mut item = draft.item.clone();
        item.transform = draft.transform;
        let rect = geometry(&item, &draft.source, None).unwrap().rect;
        assert_eq!((rect.x, rect.y), (original.x, original.y));
    }
    fn fixture() -> (tokio::runtime::Runtime, prismcast_app::AppHandle, SceneId) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let handle = runtime.block_on(async {
            let handle = prismcast_app::AppHandle::spawn(prismcast_app::CoreConfig::default());
            handle
                .dispatch(Command::AddScene {
                    name: "Editing".into(),
                })
                .await
                .unwrap();
            handle
                .dispatch(Command::AddSource {
                    kind: prismcast_core::SourceKind::TestPattern,
                    name: "Pattern".into(),
                })
                .await
                .unwrap();
            let scene_id = handle.snapshot().current_scene().unwrap();
            let source_id = handle.snapshot().sources().next().unwrap().id;
            handle
                .dispatch(Command::SetSourceSettings {
                    source_id,
                    settings: serde_json::json!({"width":100,"height":50}),
                })
                .await
                .unwrap();
            for _ in 0..2 {
                handle
                    .dispatch(Command::AddSceneItem {
                        scene_id,
                        source_id,
                    })
                    .await
                    .unwrap();
            }
            handle
        });
        let scene_id = handle.snapshot().current_scene().unwrap();
        (runtime, handle, scene_id)
    }
    #[test]
    fn topmost_hit_motion_is_local_and_one_end_command_commits() {
        let (runtime, handle, scene) = fixture();
        let snapshot = handle.snapshot();
        let top = snapshot.scene(scene).unwrap().items[1].id;
        let mut state = State {
            available: true,
            ..State::default()
        };
        state.refresh(snapshot.clone());
        state.begin(20.0, 20.0, 1920.0, 1080.0);
        assert_eq!(state.selected, Some(top));
        for i in 0..1000 {
            state.draft.as_mut().unwrap().update(i as f64, 5.0);
        }
        assert_eq!(handle.snapshot().revision(), snapshot.revision());
        let command = state.finish(30.0, 20.0).unwrap();
        runtime.block_on(handle.dispatch(command)).unwrap();
        assert_eq!(handle.snapshot().revision(), snapshot.revision() + 1);
        assert!(state.finish(40.0, 30.0).is_none());
        runtime.block_on(handle.shutdown());
    }
    #[test]
    fn remote_lock_and_profile_changes_cancel_stale_drafts() {
        let (runtime, handle, scene) = fixture();
        let mut state = State {
            available: true,
            ..State::default()
        };
        state.refresh(handle.snapshot());
        state.begin(20.0, 20.0, 1920.0, 1080.0);
        let item_id = state.selected.unwrap();
        runtime
            .block_on(handle.dispatch(Command::SetSceneItemLocked {
                scene_id: scene,
                item_id,
                locked: true,
            }))
            .unwrap();
        state.refresh(handle.snapshot());
        assert!(state.finish(20.0, 20.0).is_none());
        state.begin(20.0, 20.0, 1920.0, 1080.0);
        assert!(state.draft.is_none());
        let profile = prismcast_core::Profile::new(
            "New canvas",
            VideoConfig {
                width: 640,
                height: 480,
                fps_num: 30,
                fps_den: 1,
            },
        );
        let profile_id = profile.id;
        runtime
            .block_on(handle.dispatch(Command::AddProfile { profile }))
            .unwrap();
        runtime
            .block_on(handle.dispatch(Command::SelectProfile { profile_id }))
            .unwrap();
        state.refresh(handle.snapshot());
        assert_eq!(state.selected, None);
        runtime.block_on(handle.shutdown());
    }

    #[test]
    #[ignore = "requires real GTK display; run this filter with --ignored --test-threads=1"]
    fn production_preview_gesture_signals_commit_once_and_cancel_stale_edits() {
        gtk::init().unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let handle = {
            let _enter = runtime.enter();
            prismcast_app::AppHandle::spawn(prismcast_app::CoreConfig::default())
        };
        gtk::glib::MainContext::default().block_on(async {
            handle
                .dispatch(Command::AddScene {
                    name: "Gesture test".into(),
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
                .dispatch(Command::SetSourceSettings {
                    source_id,
                    settings: serde_json::json!({"width":100,"height":50}),
                })
                .await
                .unwrap();
            handle
                .dispatch(Command::AddSceneItem {
                    scene_id,
                    source_id,
                })
                .await
                .unwrap();
            let commands = Rc::new(RefCell::new(Vec::<Command>::new()));
            let editor = PreviewEditor::new(
                &gtk::Picture::new(),
                {
                    let handle = handle.clone();
                    move || handle.snapshot()
                },
                {
                    let commands = commands.clone();
                    move |command| commands.borrow_mut().push(command)
                },
            );
            editor.refresh(handle.snapshot());
            editor.set_available(true);
            let window = gtk::Window::new();
            window.set_default_size(700, 700);
            window.set_child(Some(editor.widget()));
            window.present();
            gtk::glib::timeout_future(std::time::Duration::from_millis(80)).await;
            let map = Mapping::new(
                editor.0.area.width() as f64,
                editor.0.area.height() as f64,
                video(&handle.snapshot()),
            )
            .unwrap();
            editor.0.gesture.emit_by_name::<()>(
                "drag-begin",
                &[&(map.x + 20.0 * map.scale), &(map.y + 20.0 * map.scale)],
            );
            let revision = handle.snapshot().revision();
            gtk::glib::timeout_future(std::time::Duration::from_millis(80)).await;
            assert!(
                editor.0.state.borrow().draft.is_some(),
                "selection layout must not cancel a stable-window drag"
            );
            for i in 0..1000 {
                editor
                    .0
                    .gesture
                    .emit_by_name::<()>("drag-update", &[&(i as f64 * 0.01), &0.0f64]);
            }
            assert!(commands.borrow().is_empty());
            editor
                .0
                .gesture
                .emit_by_name::<()>("drag-end", &[&(30.0 * map.scale), &(20.0 * map.scale)]);
            assert_eq!(commands.borrow().len(), 1);
            let command = commands.borrow_mut().pop().unwrap();
            handle.dispatch(command).await.unwrap();
            editor.completed(handle.snapshot());
            assert_eq!(handle.snapshot().revision(), revision + 1);
            let item_id = handle.snapshot().scene(scene_id).unwrap().items[0].id;
            assert_eq!(
                handle.snapshot().scene(scene_id).unwrap().items[0]
                    .transform
                    .position,
                prismcast_core::Vec2::new(30.0, 20.0)
            );
            editor.0.gesture.emit_by_name::<()>(
                "drag-begin",
                &[&(map.x + 40.0 * map.scale), &(map.y + 30.0 * map.scale)],
            );
            handle
                .dispatch(Command::SetSceneItemLocked {
                    scene_id,
                    item_id,
                    locked: true,
                })
                .await
                .unwrap();
            // Deliberately omit editor.refresh: end must preflight latest core.
            editor
                .0
                .gesture
                .emit_by_name::<()>("drag-end", &[&30.0f64, &20.0f64]);
            assert!(commands.borrow().is_empty());
            handle
                .dispatch(Command::SetSceneItemLocked {
                    scene_id,
                    item_id,
                    locked: false,
                })
                .await
                .unwrap();
            editor.refresh(handle.snapshot());
            editor.0.gesture.emit_by_name::<()>(
                "drag-begin",
                &[&(map.x + 40.0 * map.scale), &(map.y + 30.0 * map.scale)],
            );
            editor
                .0
                .gesture
                .emit_by_name::<()>("cancel", &[&None::<gtk::gdk::EventSequence>]);
            editor
                .0
                .gesture
                .emit_by_name::<()>("drag-end", &[&30.0f64, &20.0f64]);
            assert!(commands.borrow().is_empty());
            // The real resize-handle signal path also sends one final intent.
            editor.0.gesture.emit_by_name::<()>(
                "drag-begin",
                &[&(map.x + 129.0 * map.scale), &(map.y + 69.0 * map.scale)],
            );
            assert!(editor.0.state.borrow().draft.as_ref().unwrap().resize);
            editor
                .0
                .gesture
                .emit_by_name::<()>("drag-end", &[&(20.0 * map.scale), &(10.0 * map.scale)]);
            let command = commands.borrow_mut().pop().unwrap();
            handle.dispatch(command).await.unwrap();
            editor.completed(handle.snapshot());
            assert_eq!(
                handle.snapshot().scene(scene_id).unwrap().items[0]
                    .transform
                    .scale,
                prismcast_core::Vec2::new(1.2, 1.2)
            );
            // Exercise keyboard-accessible production button signals too.
            let click = |label: &str| {
                let mut child = editor.0.controls.first_child();
                while let Some(widget) = child {
                    if let Ok(button) = widget.clone().downcast::<gtk::Button>() {
                        if button.label().as_deref() == Some(label) {
                            button.emit_clicked();
                            return;
                        }
                    }
                    child = widget.next_sibling();
                }
                panic!("missing production control {label}");
            };
            handle
                .dispatch(Command::AddSceneItem {
                    scene_id,
                    source_id,
                })
                .await
                .unwrap();
            editor.refresh(handle.snapshot());
            let other_item = handle.snapshot().scene(scene_id).unwrap().items[1].id;
            editor.0.selector.set_selected(2);
            assert_eq!(editor.0.state.borrow().selected, Some(other_item));
            editor.0.selector.set_selected(1);
            assert_eq!(editor.0.state.borrow().selected, Some(item_id));
            editor.0.fields[0].set_value(55.0);
            editor.0.fields[1].set_value(66.0);
            editor.0.fields[2].set_value(1.0);
            editor.0.fields[3].set_value(1.0);
            click("Apply");
            let command = commands.borrow_mut().pop().unwrap();
            handle.dispatch(command).await.unwrap();
            editor.completed(handle.snapshot());
            assert_eq!(
                handle.snapshot().scene(scene_id).unwrap().items[0]
                    .transform
                    .position,
                prismcast_core::Vec2::new(55.0, 66.0)
            );
            click("Rotate 90°");
            let command = commands.borrow_mut().pop().unwrap();
            handle.dispatch(command).await.unwrap();
            editor.completed(handle.snapshot());
            assert_eq!(
                handle.snapshot().scene(scene_id).unwrap().items[0]
                    .transform
                    .rotation,
                90.0
            );
            click("Flip X");
            let command = commands.borrow_mut().pop().unwrap();
            handle.dispatch(command).await.unwrap();
            editor.completed(handle.snapshot());
            assert_eq!(
                handle.snapshot().scene(scene_id).unwrap().items[0]
                    .transform
                    .scale
                    .x,
                -1.0
            );
            handle
                .dispatch(Command::SetSceneItemBounds {
                    scene_id,
                    item_id,
                    bounds: prismcast_core::Bounds {
                        kind: BoundsKind::Stretch,
                        size: prismcast_core::Vec2::new(200.0, 100.0),
                        alignment: prismcast_core::Anchor::TopLeft,
                    },
                })
                .await
                .unwrap();
            editor.refresh(handle.snapshot());
            assert!(!editor.0.fields[2].is_sensitive());
            assert!(!editor.0.fields[3].is_sensitive());
            editor.0.gesture.emit_by_name::<()>(
                "drag-begin",
                &[&(map.x + 70.0 * map.scale), &(map.y + 80.0 * map.scale)],
            );
            window.set_default_size(1000, 800);
            gtk::glib::timeout_future(std::time::Duration::from_millis(100)).await;
            editor
                .0
                .gesture
                .emit_by_name::<()>("drag-end", &[&30.0f64, &20.0f64]);
            assert!(commands.borrow().is_empty());
            handle
                .dispatch(Command::SetSceneItemBounds {
                    scene_id,
                    item_id,
                    bounds: prismcast_core::Bounds::default(),
                })
                .await
                .unwrap();
            handle
                .dispatch(Command::SetSourceSettings {
                    source_id,
                    settings: serde_json::json!({"width":8192,"height":50}),
                })
                .await
                .unwrap();
            editor.refresh(handle.snapshot());
            editor.0.fields[3].set_value(64.0);
            click("Apply");
            assert!(commands.borrow().is_empty());
            assert!(editor.0.hint.label().contains("Cannot apply"));
            editor.set_available(false);
            assert!(!editor.0.controls.is_sensitive());
            assert!(!editor.0.selector.is_sensitive());
            window.close();
            handle.shutdown().await;
        });
    }
}
