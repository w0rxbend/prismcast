//! Window actions render advisory Core history metadata; replay stays in Core.

use adw::prelude::*;
use prismcast_app::HistoryStatus;
use prismcast_core::Command;

pub(crate) struct HistoryControls {
    undo: gtk::gio::SimpleAction,
    redo: gtk::gio::SimpleAction,
    undo_button: gtk::Button,
    redo_button: gtk::Button,
}

impl HistoryControls {
    pub(crate) fn new(
        window: &adw::ApplicationWindow,
        header: &adw::HeaderBar,
        dispatch: impl Fn(Command) + Clone + 'static,
    ) -> Self {
        let undo = gtk::gio::SimpleAction::new("undo", None);
        let redo = gtk::gio::SimpleAction::new("redo", None);
        undo.set_enabled(false);
        redo.set_enabled(false);
        undo.connect_activate({
            let dispatch = dispatch.clone();
            move |_, _| dispatch(Command::Undo)
        });
        redo.connect_activate(move |_, _| dispatch(Command::Redo));
        window.add_action(&undo);
        window.add_action(&redo);
        let undo_button = gtk::Button::from_icon_name("edit-undo-symbolic");
        undo_button.set_action_name(Some("win.undo"));
        let redo_button = gtk::Button::from_icon_name("edit-redo-symbolic");
        redo_button.set_action_name(Some("win.redo"));
        header.pack_start(&undo_button);
        header.pack_start(&redo_button);

        // Bubble after child editors, with an explicit focus guard even when
        // their own undo stack is empty or editing has been made read-only.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Bubble);
        keys.connect_key_pressed({
            let window = window.downgrade();
            let undo = undo.clone();
            let redo = redo.clone();
            move |_, key, _, modifiers| {
                let Some(window) = window.upgrade() else {
                    return gtk::glib::Propagation::Proceed;
                };
                let focus = gtk::prelude::GtkWindowExt::focus(&window);
                let Some(redo_key) = history_shortcut(key, modifiers, focus) else {
                    return gtk::glib::Propagation::Proceed;
                };
                let action = if redo_key { &redo } else { &undo };
                if !action.is_enabled() {
                    return gtk::glib::Propagation::Proceed;
                }
                action.activate(None);
                gtk::glib::Propagation::Stop
            }
        });
        window.add_controller(keys);
        let controls = Self {
            undo,
            redo,
            undo_button,
            redo_button,
        };
        controls.refresh(&HistoryStatus::default(), false);
        controls
    }

    pub(crate) fn refresh(&self, history: &HistoryStatus, shutting_down: bool) {
        self.undo.set_enabled(!shutting_down && history.can_undo());
        self.redo.set_enabled(!shutting_down && history.can_redo());
        for (button, verb, label, shortcut) in [
            (&self.undo_button, "Undo", &history.undo_label, "Ctrl+Z"),
            (
                &self.redo_button,
                "Redo",
                &history.redo_label,
                "Ctrl+Shift+Z",
            ),
        ] {
            let description = if history.group_open {
                format!("{verb} unavailable while an edit group is open")
            } else {
                label.as_ref().map_or_else(
                    || format!("Nothing to {}", verb.to_lowercase()),
                    |label| format!("{verb} {label} ({shortcut})"),
                )
            };
            button.set_tooltip_text(Some(&description));
        }
    }
}

fn history_shortcut(
    key: gtk::gdk::Key,
    modifiers: gtk::gdk::ModifierType,
    mut focus: Option<gtk::Widget>,
) -> Option<bool> {
    while let Some(widget) = focus {
        if widget.is::<gtk::Editable>() || widget.is::<gtk::TextView>() {
            return None;
        }
        focus = widget.parent();
    }
    let relevant = modifiers & !gtk::gdk::ModifierType::LOCK_MASK;
    let control = gtk::gdk::ModifierType::CONTROL_MASK;
    let redo = control | gtk::gdk::ModifierType::SHIFT_MASK;
    if key != gtk::gdk::Key::z && key != gtk::gdk::Key::Z {
        None
    } else if relevant == control {
        Some(false)
    } else if relevant == redo {
        Some(true)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_requires_control_z_with_only_optional_shift_and_caps_lock() {
        use gtk::gdk::{Key, ModifierType as M};
        assert_eq!(history_shortcut(Key::z, M::CONTROL_MASK, None), Some(false));
        assert_eq!(
            history_shortcut(Key::Z, M::CONTROL_MASK | M::SHIFT_MASK | M::LOCK_MASK, None),
            Some(true)
        );
        for modifiers in [
            M::empty(),
            M::SHIFT_MASK,
            M::CONTROL_MASK | M::ALT_MASK,
            M::CONTROL_MASK | M::SUPER_MASK,
        ] {
            assert_eq!(history_shortcut(Key::z, modifiers, None), None);
        }
        assert_eq!(history_shortcut(Key::y, M::CONTROL_MASK, None), None);
    }

    #[test]
    #[ignore = "requires real GTK display; run separately with --ignored --test-threads=1"]
    fn text_editors_keep_history_shortcuts_even_readonly_or_empty() {
        adw::init().unwrap();
        let app = adw::Application::builder()
            .application_id("io.github.worxbend.prismcast.historyfocus")
            .build();
        app.register(None::<&gtk::gio::Cancellable>).unwrap();
        let window = adw::ApplicationWindow::new(&app);
        let header = adw::HeaderBar::new();
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let history = HistoryControls::new(&window, &header, {
            let calls = calls.clone();
            move |command| calls.borrow_mut().push(command)
        });
        history.refresh(
            &HistoryStatus {
                undo_label: Some("rename source".into()),
                redo_label: Some("transform scene item".into()),
                group_open: false,
            },
            false,
        );
        let entry = gtk::Entry::new();
        let spin = gtk::SpinButton::with_range(0.0, 100.0, 1.0);
        let password = gtk::PasswordEntry::new();
        let text_view = gtk::TextView::new();
        let controls: Vec<gtk::Widget> = vec![
            entry.clone().upcast(),
            spin.upcast(),
            password.upcast(),
            text_view.upcast(),
        ];
        for widget in controls {
            assert_eq!(
                history_shortcut(
                    gtk::gdk::Key::z,
                    gtk::gdk::ModifierType::CONTROL_MASK,
                    Some(widget.clone())
                ),
                None
            );
            if let Some(editable) = widget.dynamic_cast_ref::<gtk::Editable>() {
                editable.set_editable(false);
            }
            assert_eq!(
                history_shortcut(
                    gtk::gdk::Key::Z,
                    gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::SHIFT_MASK,
                    Some(widget)
                ),
                None
            );
        }
        let child = entry.first_child().unwrap();
        assert!(
            child.is::<gtk::Text>(),
            "actual Entry delegate should be guarded"
        );
        assert_eq!(
            history_shortcut(
                gtk::gdk::Key::z,
                gtk::gdk::ModifierType::CONTROL_MASK,
                Some(child)
            ),
            None
        );
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&header);
        content.append(&entry);
        window.set_content(Some(&content));
        window.present();
        let context = gtk::glib::MainContext::default();
        while context.pending() {
            context.iteration(false);
        }
        let keys = window
            .observe_controllers()
            .iter::<gtk::glib::Object>()
            .filter_map(Result::ok)
            .find_map(|controller| controller.downcast::<gtk::EventControllerKey>().ok())
            .unwrap();
        let emit =
            |key, modifiers| keys.emit_by_name::<bool>("key-pressed", &[&key, &0_u32, &modifiers]);
        gtk::prelude::GtkWindowExt::set_focus(&window, Some(&entry));
        assert!(!emit(
            gtk::gdk::Key::z,
            gtk::gdk::ModifierType::CONTROL_MASK
        ));
        assert!(
            calls.borrow().is_empty(),
            "actual controller must preserve readonly Entry shortcut"
        );
        gtk::prelude::GtkWindowExt::set_focus(&window, Some(&history.undo_button));
        assert!(emit(gtk::gdk::Key::z, gtk::gdk::ModifierType::CONTROL_MASK));
        assert!(emit(
            gtk::gdk::Key::Z,
            gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::SHIFT_MASK
        ));
        assert!(matches!(
            calls.borrow().as_slice(),
            [Command::Undo, Command::Redo]
        ));
        history.refresh(
            &HistoryStatus {
                undo_label: Some("rename source".into()),
                redo_label: None,
                group_open: false,
            },
            true,
        );
        assert!(!emit(
            gtk::gdk::Key::z,
            gtk::gdk::ModifierType::CONTROL_MASK
        ));
        assert_eq!(calls.borrow().len(), 2);
        window.close();
    }
}
