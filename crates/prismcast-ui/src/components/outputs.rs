//! The outputs panel: one card per configured output with start/stop
//! controls, plus an add button, in the bottom bar per PLAN.md §28.

use std::sync::Arc;

use adw::prelude::*;
use prismcast_app::AppSnapshot;
use prismcast_core::id::OutputId;
use relm4::{ComponentParts, ComponentSender, SimpleComponent};

use crate::presentation::{output_can_start, output_can_stop, output_state_label};

/// The outputs panel. Rebuilds its cards from each snapshot refresh.
pub struct OutputsPanel {
    cards: gtk::Box,
}

#[derive(Debug)]
pub enum OutputsInput {
    /// A fresh snapshot; the cards are rebuilt from it.
    Refresh(Arc<AppSnapshot>),
}

#[derive(Debug)]
pub enum OutputsOutput {
    /// The user asked to start an output.
    StartRequested(OutputId),
    /// The user asked to stop an output.
    StopRequested(OutputId),
    /// The user clicked the add button; the root should add an output.
    AddRequested,
}

impl SimpleComponent for OutputsPanel {
    type Input = OutputsInput;
    type Output = OutputsOutput;
    type Init = ();
    type Root = gtk::Frame;
    type Widgets = ();

    fn init_root() -> Self::Root {
        gtk::Frame::new(Some("Outputs"))
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        root.set_hexpand(true);

        let container = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        container.set_margin_start(6);
        container.set_margin_end(6);
        container.set_margin_top(6);
        container.set_margin_bottom(6);

        let cards = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        cards.set_hexpand(true);
        container.append(&cards);

        let add_button = gtk::Button::with_label("+");
        add_button.set_tooltip_text(Some("Add recording output"));
        container.append(&add_button);
        root.set_child(Some(&container));

        add_button.connect_clicked(move |_| {
            sender.output_sender().emit(OutputsOutput::AddRequested);
        });

        let model = Self { cards };
        ComponentParts { model, widgets: () }
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>) {
        match message {
            OutputsInput::Refresh(snapshot) => self.refresh(&snapshot, &sender),
        }
    }
}

impl OutputsPanel {
    /// Rebuilds one card per output: name, lifecycle state, and start/stop
    /// buttons gated on the state's legal transitions.
    fn refresh(&mut self, snapshot: &AppSnapshot, sender: &ComponentSender<Self>) {
        while let Some(child) = self.cards.first_child() {
            self.cards.remove(&child);
        }
        for output in snapshot.outputs() {
            let card = gtk::Box::new(gtk::Orientation::Vertical, 4);
            card.set_margin_start(6);
            card.set_margin_end(6);
            card.set_margin_top(6);
            card.set_margin_bottom(6);

            let name = gtk::Label::new(Some(&output.name));
            name.add_css_class("heading");
            card.append(&name);

            let state = gtk::Label::new(Some(&output_state_label(&output.state)));
            state.add_css_class("dim-label");
            state.add_css_class("caption");
            card.append(&state);

            let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            buttons.set_halign(gtk::Align::Center);

            let start = gtk::Button::with_label("Start");
            start.add_css_class("suggested-action");
            start.set_sensitive(output_can_start(&output.state));
            {
                let out = sender.output_sender().clone();
                let output_id = output.id;
                start.connect_clicked(move |_| {
                    out.emit(OutputsOutput::StartRequested(output_id));
                });
            }
            buttons.append(&start);

            let stop = gtk::Button::with_label("Stop");
            stop.add_css_class("destructive-action");
            stop.set_sensitive(output_can_stop(&output.state));
            {
                let out = sender.output_sender().clone();
                let output_id = output.id;
                stop.connect_clicked(move |_| {
                    out.emit(OutputsOutput::StopRequested(output_id));
                });
            }
            buttons.append(&stop);

            card.append(&buttons);

            let frame = gtk::Frame::new(None);
            frame.set_child(Some(&card));
            self.cards.append(&frame);
        }
    }
}
