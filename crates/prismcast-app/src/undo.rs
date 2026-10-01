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
//! an entity that has since been removed); a failed undo preserves the entry and
//! surfaces the error. Snapshot-restore-based undo for destructive cascades
//! is a documented follow-up of the domain crate.
//!
//! Authorization is checked recursively by the actor before an inverse applies.
//! Failed undo/redo preserves the entry. Destructive restoration is deferred.

use prismcast_core::error::{Error, Result};
use prismcast_core::Command;

/// Default maximum number of entries kept on the undo stack (oldest dropped).
pub const DEFAULT_UNDO_CAPACITY: usize = 100;

/// Explicit undo budgets; serialized bytes are a conservative retained-payload measure.
#[derive(Debug, Clone, Copy)]
pub struct UndoLimits {
    /// Total retained inverse/label bytes, including an open group.
    pub retained_bytes: usize,
    /// Maximum inverses in one open group or leaves in a transaction.
    pub group_members: usize,
    /// Maximum UTF-8 label bytes.
    pub label_bytes: usize,
    /// Maximum nested transaction depth.
    pub nesting: usize,
}
impl Default for UndoLimits {
    fn default() -> Self {
        Self {
            retained_bytes: 8 * 1024 * 1024,
            group_members: 256,
            label_bytes: 256,
            nesting: 16,
        }
    }
}
fn limit_error() -> Error {
    Error::InvalidInput("undo history resource limit exceeded".into())
}

/// Counts serialization bytes with no payload buffer and stops at the budget.
pub(crate) fn bounded_size(value: &impl serde::Serialize, budget: usize) -> Result<usize> {
    struct Counter {
        count: usize,
        budget: usize,
    }
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.budget.saturating_sub(self.count) {
                return Err(std::io::Error::other("undo byte budget exceeded"));
            }
            self.count += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter { count: 0, budget };
    serde_json::to_writer(&mut counter, value).map_err(|_| limit_error())?;
    Ok(counter.count)
}
fn entry_bytes(entry: &UndoEntry, budget: usize) -> usize {
    command_bytes(&entry.inverse, budget)
        .unwrap_or(budget)
        .saturating_add(entry.label.len())
        .saturating_add(128)
}
/// Bounds recursion and leaf count before recursive authorization/inverse generation.
pub(crate) fn validate_structure(command: &Command, limits: UndoLimits) -> Result<()> {
    fn walk(command: &Command, depth: usize, leaves: &mut usize, limits: UndoLimits) -> Result<()> {
        if depth > limits.nesting.min(64) {
            return Err(limit_error());
        }
        if let Command::Transaction { commands } = command {
            *leaves = leaves.saturating_add(1);
            for command in commands {
                walk(command, depth + 1, leaves, limits)?;
            }
        } else {
            *leaves = leaves.saturating_add(1);
            match command {
                Command::SetSourceSettings { settings, .. } => validate_json(settings, limits)?,
                Command::SetTransition { transition } => {
                    validate_json(&transition.settings, limits)?
                }
                Command::AddProfile { profile } => validate_json(&profile.settings, limits)?,
                _ => {}
            }
        }
        if *leaves > limits.group_members {
            return Err(limit_error());
        }
        Ok(())
    }
    walk(command, 0, &mut 0, limits)
}

/// JSON recursion is capped before serde traversal or inverse cloning.
pub(crate) fn validate_json(value: &serde_json::Value, limits: UndoLimits) -> Result<()> {
    fn walk(
        value: &serde_json::Value,
        depth: usize,
        nodes: &mut usize,
        limits: UndoLimits,
    ) -> Result<()> {
        *nodes = nodes.saturating_add(1);
        if depth > limits.nesting.min(64) || *nodes > limits.retained_bytes {
            return Err(limit_error());
        }
        match value {
            serde_json::Value::Array(values) => {
                for value in values {
                    walk(value, depth + 1, nodes, limits)?;
                }
            }
            serde_json::Value::Object(values) => {
                for value in values.values() {
                    walk(value, depth + 1, nodes, limits)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    walk(value, 0, &mut 0, limits)
}
fn command_bytes(command: &Command, budget: usize) -> Result<usize> {
    let bytes = bounded_size(command, budget)?
        .saturating_add(command_members(command).saturating_mul(std::mem::size_of::<Command>()));
    if bytes > budget {
        return Err(limit_error());
    }
    Ok(bytes)
}
// Entry bookkeeping plus ample serialized Transaction framing and wrapper storage.
const GROUP_OVERHEAD: usize = 256 + std::mem::size_of::<Command>();

fn command_members(command: &Command) -> usize {
    match command {
        Command::Transaction { commands } => {
            1usize.saturating_add(commands.iter().map(command_members).sum::<usize>())
        }
        _ => 1,
    }
}

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
    limits: UndoLimits,
}

struct OpenGroup {
    label: String,
    /// Inverses in apply order; reversed when the group closes.
    inverses: Vec<Command>,
    bytes: usize,
    members: usize,
}

impl UndoService {
    /// Creates an empty service with the given stack capacity.
    pub fn new(capacity: usize) -> Self {
        Self::with_limits(capacity, UndoLimits::default())
    }

    /// Creates history with explicit resource budgets.
    pub fn with_limits(capacity: usize, limits: UndoLimits) -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            open_group: None,
            capacity: capacity.max(1),
            limits,
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
        let label = label.into();
        if label.len() > self.limits.label_bytes
            || label.len().saturating_add(GROUP_OVERHEAD) > self.limits.retained_bytes
        {
            return Err(limit_error());
        }
        self.open_group = Some(OpenGroup {
            bytes: label.len().saturating_add(GROUP_OVERHEAD),
            label,
            inverses: Vec::new(),
            members: 0,
        });
        while self.retained_bytes() > self.limits.retained_bytes {
            if !self.undo.is_empty() {
                self.undo.remove(0);
            } else if !self.redo.is_empty() {
                self.redo.remove(0);
            } else {
                break;
            }
        }
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
        if self.preflight(label, inverse.as_ref(), true).is_err() {
            return;
        }
        self.redo.clear();
        let Some(inverse) = inverse else {
            return;
        };
        if let Some(group) = &mut self.open_group {
            group.members += command_members(&inverse);
            group.bytes += 1 + command_bytes(&inverse, self.limits.retained_bytes)
                .unwrap_or(self.limits.retained_bytes);
            group.inverses.push(inverse);
            while self.retained_bytes() > self.limits.retained_bytes && !self.undo.is_empty() {
                self.undo.remove(0);
            }
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
        if self
            .preflight(&entry.label, Some(&entry.inverse), false)
            .is_err()
        {
            return;
        }
        self.make_room(&entry);
        self.redo.push(entry);
    }

    /// Returns an entry to the undo stack after a failed undo attempt.
    pub fn push_undo_back(&mut self, entry: UndoEntry) {
        if self
            .preflight(&entry.label, Some(&entry.inverse), false)
            .is_err()
        {
            return;
        }
        self.make_room(&entry);
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

    /// Validates a prospective retained inverse without changing history.
    /// `joins_group` is false for a successful foreign controller boundary.
    pub fn preflight(
        &self,
        label: &str,
        inverse: Option<&Command>,
        joins_group: bool,
    ) -> Result<()> {
        if label.len() > self.limits.label_bytes {
            return Err(limit_error());
        }
        let Some(inverse) = inverse else {
            return Ok(());
        };
        validate_structure(inverse, self.limits)?;
        let bytes = command_bytes(inverse, self.limits.retained_bytes)?;
        if joins_group {
            if let Some(group) = &self.open_group {
                if self.limits.nesting == 0 && !group.inverses.is_empty() {
                    return Err(limit_error());
                }
                let mut grouped_limits = self.limits;
                grouped_limits.nesting = grouped_limits.nesting.min(64).saturating_sub(1);
                validate_structure(inverse, grouped_limits)?;
                if group
                    .members
                    .saturating_add(command_members(inverse))
                    .saturating_add(1)
                    > self.limits.group_members
                    || group.bytes.saturating_add(bytes).saturating_add(1)
                        > self.limits.retained_bytes
                {
                    return Err(limit_error());
                }
                return Ok(());
            }
        }
        if bytes.saturating_add(label.len()).saturating_add(128) > self.limits.retained_bytes {
            return Err(limit_error());
        }
        Ok(())
    }

    /// Peeks without taking ownership so failed authorization preserves history.
    pub fn next_undo(&self) -> Option<&UndoEntry> {
        self.undo.last()
    }
    /// Peeks at the next redo step.
    pub fn next_redo(&self) -> Option<&UndoEntry> {
        self.redo.last()
    }
    fn retained_bytes(&self) -> usize {
        self.undo
            .iter()
            .chain(&self.redo)
            .map(|e| entry_bytes(e, self.limits.retained_bytes))
            .sum::<usize>()
            + self.open_group.as_ref().map_or(0, |g| g.bytes)
    }
    fn make_room(&mut self, entry: &UndoEntry) {
        let bytes = entry_bytes(entry, self.limits.retained_bytes);
        while self.retained_bytes().saturating_add(bytes) > self.limits.retained_bytes {
            if !self.undo.is_empty() {
                self.undo.remove(0);
            } else if !self.redo.is_empty() {
                self.redo.remove(0);
            } else {
                break;
            }
        }
    }

    fn push_undo(&mut self, entry: UndoEntry) {
        let bytes = entry_bytes(&entry, self.limits.retained_bytes);
        while !self.undo.is_empty()
            && (self.undo.len() >= self.capacity
                || self.retained_bytes().saturating_add(bytes) > self.limits.retained_bytes)
        {
            self.undo.remove(0);
        }
        self.make_room(&entry);
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
    fn zero_nesting_group_remains_closable_after_second_member_rejection() {
        let limits = UndoLimits {
            nesting: 0,
            ..UndoLimits::default()
        };
        let mut undo = UndoService::with_limits(10, limits);
        undo.begin_transaction("single").unwrap();
        undo.record("first", Some(cmd("one")));
        assert!(undo.preflight("second", Some(&cmd("two")), true).is_err());
        undo.end_transaction().unwrap();
        validate_structure(&undo.next_undo().unwrap().inverse, limits).unwrap();
    }

    #[test]
    fn total_byte_budget_is_preserved_while_group_grows_and_closes() {
        let limits = UndoLimits {
            retained_bytes: 2500,
            ..UndoLimits::default()
        };
        let mut undo = UndoService::with_limits(10, limits);
        undo.record("old one", Some(cmd(&"x".repeat(500))));
        undo.record("old two", Some(cmd(&"x".repeat(500))));
        undo.begin_transaction("group").unwrap();
        assert!(undo.retained_bytes() <= limits.retained_bytes);
        for _ in 0..2 {
            let inverse = cmd(&"y".repeat(500));
            undo.preflight("rename scene", Some(&inverse), true)
                .unwrap();
            undo.record("rename scene", Some(inverse));
            assert!(undo.retained_bytes() <= limits.retained_bytes);
        }
        let before = undo.retained_bytes();
        assert!(undo
            .preflight("rename scene", Some(&cmd(&"z".repeat(2000))), true)
            .is_err());
        assert_eq!(undo.retained_bytes(), before);
        undo.end_transaction().unwrap();
        assert!(undo.retained_bytes() <= limits.retained_bytes);
        assert_eq!(undo.undo_label(), Some("group"));
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
