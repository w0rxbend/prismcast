//! Undo/redo stacks driven by command inverses (PLAN.md §59, ADR-0005).
//!
//! The core actor owns an [`UndoService`]. Before applying a command it asks
//! [`AppState::inverse`] for the inverse command (reconstructed from the
//! pre-application state); on success the inverse is recorded here. `Undo`
//! and `Redo` are themselves dispatched through the actor, so undoing emits
//! normal events and publishes a normal snapshot — controllers cannot tell an
//! undo apart from any other command (ADR-0005's invariant holds).
//!
//! ## Transaction grouping (PLAN.md §59)
//!
//! `begin_transaction(label)` / `end_transaction()` group the inverses of all
//! commands applied in between into a single undo entry — a drag gesture
//! becomes one undo step instead of hundreds. (This is distinct from
//! [`Command::Transaction`], which is *atomic application*; grouping here is
//! about undo granularity for separate commands.)
//!
//! ## Known limitations (from ARCH-002, unchanged by this crate)
//!
//! `Add*` / `Remove*` / `DuplicateSceneItem` inverses return `None` from
//! [`AppState::inverse`], so creations and destructive removals are currently
//! **not undoable**. They do not clear the existing undo stack, but undoing
//! across such an operation can fail at apply time (e.g. the inverse targets
//! an entity that has since been removed); a failed undo drops the entry and
//! surfaces the error. Snapshot-restore-based undo for destructive cascades
//! is a documented follow-up of the domain crate.
//!
//! ## Authorization
//!
//! Undo/redo mutate arbitrary domains, so per-domain permission mapping does
//! not apply. Interim policy: any caller that can control anything
//! ([`crate::dispatch::Permissions::can_control`]) may undo/redo; read-only
//! callers may not.
//! Fine-grained undo authz is left to the follow-up undo task (PLAN.md §61
//! lists `CORE-005 undo/redo`).

use prismcast_core::error::{Error, Result};
use prismcast_core::Command;

/// Default maximum number of entries kept on the undo stack (oldest dropped).
pub const DEFAULT_UNDO_CAPACITY: usize = 100;

/// One recorded undoable step.
#[derive(Debug, Clone)]
pub struct UndoEntry {
    /// Human-readable label (from [`Command::label`], or the group label).
    pub label: String,
    /// Command that reverses the step when applied to the post-step state.
    pub inverse: Command,
}

/// Undo/redo stacks plus open transaction-group state. Pure bookkeeping — the
/// actor drives it and applies the commands it hands out.
pub struct UndoService {
    undo: Vec<UndoEntry>,
    redo: Vec<UndoEntry>,
    open_group: Option<OpenGroup>,
    capacity: usize,
}

struct OpenGroup {
    label: String,
    /// Inverses in apply order; reversed when the group closes.
    inverses: Vec<Command>,
}

impl UndoService {
    /// Creates an empty service with the given stack capacity.
    pub fn new(capacity: usize) -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            open_group: None,
            capacity: capacity.max(1),
        }
    }

    /// Whether a transaction group is currently open.
    pub fn in_transaction(&self) -> bool {
        self.open_group.is_some()
    }

    /// Opens a transaction group. Nested groups are rejected.
    pub fn begin_transaction(&mut self, label: impl Into<String>) -> Result<()> {
        if self.open_group.is_some() {
            return Err(Error::InvalidInput(
                "a transaction group is already open".into(),
            ));
        }
        self.open_group = Some(OpenGroup {
            label: label.into(),
            inverses: Vec::new(),
        });
        Ok(())
    }

    /// Closes the open group, recording it as one undo entry (or nothing if
    /// no undoable commands were recorded inside).
    pub fn end_transaction(&mut self) -> Result<()> {
        let group = self
            .open_group
            .take()
            .ok_or_else(|| Error::InvalidInput("no transaction group is open".into()))?;
        match group.inverses.len() {
            0 => {}
            1 => {
                let mut inverses = group.inverses;
                if let Some(inverse) = inverses.pop() {
                    self.push_undo(UndoEntry {
                        label: group.label,
                        inverse,
                    });
                }
            }
            _ => {
                let mut reversed = group.inverses;
                reversed.reverse();
                self.push_undo(UndoEntry {
                    label: group.label,
                    inverse: Command::Transaction { commands: reversed },
                });
            }
        }
        Ok(())
    }

    /// Records a successfully applied command.
    ///
    /// `inverse` is the pre-application [`AppState::inverse`] result; `None`
    /// (irreversible command) records nothing — see module docs. Recording
    /// any new command clears the redo stack.
    pub fn record(&mut self, label: &str, inverse: Option<Command>) {
        self.redo.clear();
        let Some(inverse) = inverse else {
            return;
        };
        if let Some(group) = &mut self.open_group {
            group.inverses.push(inverse);
        } else {
            self.push_undo(UndoEntry {
                label: label.to_string(),
                inverse,
            });
        }
    }

    /// Pops the next command to apply for `Undo`.
    pub fn pop_undo(&mut self) -> Option<UndoEntry> {
        self.undo.pop()
    }

    /// Pops the next command to apply for `Redo`.
    pub fn pop_redo(&mut self) -> Option<UndoEntry> {
        self.redo.pop()
    }

    /// Records a successfully applied undo as a redoable step. Does not clear
    /// anything (unlike [`record`](Self::record)).
    pub fn push_redo(&mut self, entry: UndoEntry) {
        self.redo.push(entry);
    }

    /// Returns an entry to the undo stack after a failed undo attempt.
    pub fn push_undo_back(&mut self, entry: UndoEntry) {
        self.undo.push(entry);
    }

    /// Label of the next undo step, for UI display.
    pub fn undo_label(&self) -> Option<&str> {
        if let Some(group) = &self.open_group {
            if !group.inverses.is_empty() {
                return Some(group.label.as_str());
            }
        }
        self.undo.last().map(|entry| entry.label.as_str())
    }

    /// Label of the next redo step, for UI display.
    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|entry| entry.label.as_str())
    }

    /// Depth of the undo stack (entries inside an open group not included).
    pub fn undo_depth(&self) -> usize {
        self.undo.len()
    }

    /// Depth of the redo stack.
    pub fn redo_depth(&self) -> usize {
        self.redo.len()
    }

    fn push_undo(&mut self, entry: UndoEntry) {
        if self.undo.len() >= self.capacity {
            self.undo.remove(0);
        }
        self.undo.push(entry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prismcast_core::id::SceneId;

    fn cmd(name: &str) -> Command {
        // All helper commands share one scene ID so `PartialEq` comparisons
        // between two `cmd("x")` calls work.
        static SCENE: std::sync::OnceLock<SceneId> = std::sync::OnceLock::new();
        let scene_id = *SCENE.get_or_init(SceneId::new);
        Command::RenameScene {
            scene_id,
            name: name.into(),
        }
    }

    #[test]
    fn record_pop_order_is_lifo() {
        let mut undo = UndoService::new(10);
        undo.record("first", Some(cmd("a")));
        undo.record("second", Some(cmd("b")));
        assert_eq!(undo.undo_label(), Some("second"));
        assert_eq!(undo.pop_undo().map(|e| e.label), Some("second".into()));
        assert_eq!(undo.pop_undo().map(|e| e.label), Some("first".into()));
        assert!(undo.pop_undo().is_none());
    }

    #[test]
    fn irreversible_commands_record_nothing_but_keep_stack() {
        let mut undo = UndoService::new(10);
        undo.record("kept", Some(cmd("a")));
        undo.record("add scene (irreversible)", None);
        assert_eq!(undo.undo_depth(), 1);
        assert_eq!(undo.undo_label(), Some("kept"));
    }

    #[test]
    fn new_command_clears_redo() {
        let mut undo = UndoService::new(10);
        undo.record("a", Some(cmd("a")));
        let entry = undo.pop_undo().expect("entry");
        undo.push_redo(entry);
        assert_eq!(undo.redo_depth(), 1);
        undo.record("b", Some(cmd("b")));
        assert_eq!(undo.redo_depth(), 0);
    }

    #[test]
    fn capacity_drops_oldest() {
        let mut undo = UndoService::new(2);
        undo.record("one", Some(cmd("1")));
        undo.record("two", Some(cmd("2")));
        undo.record("three", Some(cmd("3")));
        assert_eq!(undo.undo_depth(), 2);
        assert_eq!(undo.pop_undo().map(|e| e.label), Some("three".into()));
        assert_eq!(undo.pop_undo().map(|e| e.label), Some("two".into()));
    }

    #[test]
    fn transaction_group_becomes_one_entry_with_reversed_transaction_inverse() {
        let mut undo = UndoService::new(10);
        undo.begin_transaction("drag").expect("begin");
        undo.record("move 1", Some(cmd("p1")));
        undo.record("move 2", Some(cmd("p2")));
        undo.record("move 3", Some(cmd("p3")));
        undo.end_transaction().expect("end");

        assert_eq!(undo.undo_depth(), 1);
        assert_eq!(undo.undo_label(), Some("drag"));
        let entry = undo.pop_undo().expect("one entry");
        match entry.inverse {
            Command::Transaction { commands } => {
                assert_eq!(commands, vec![cmd("p3"), cmd("p2"), cmd("p1")]);
            }
            other => panic!("expected transaction inverse, got {other:?}"),
        }
    }

    #[test]
    fn single_member_group_records_bare_inverse() {
        let mut undo = UndoService::new(10);
        undo.begin_transaction("single").expect("begin");
        undo.record("only", Some(cmd("x")));
        undo.end_transaction().expect("end");
        let entry = undo.pop_undo().expect("one entry");
        assert_eq!(entry.inverse, cmd("x"));
    }

    #[test]
    fn empty_group_records_nothing_and_nesting_is_rejected() {
        let mut undo = UndoService::new(10);
        undo.begin_transaction("empty").expect("begin");
        undo.record("irreversible", None);
        undo.end_transaction().expect("end");
        assert_eq!(undo.undo_depth(), 0);

        undo.begin_transaction("outer").expect("begin");
        assert!(undo.begin_transaction("inner").is_err());
        undo.end_transaction().expect("end");
        assert!(undo.end_transaction().is_err());
    }

    #[test]
    fn group_label_visible_while_open() {
        let mut undo = UndoService::new(10);
        undo.begin_transaction("drag").expect("begin");
        assert_eq!(undo.undo_label(), None);
        undo.record("move", Some(cmd("x")));
        assert_eq!(undo.undo_label(), Some("drag"));
        undo.end_transaction().expect("end");
    }
}
