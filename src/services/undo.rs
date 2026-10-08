//! Undo / redo of file operations.
//!
//! Every completed operation that can be reverted produces an
//! [`UndoRecord`]: the operation as done (`forward`) and the operation that
//! reverts it (`inverse`), both built by the engine from what actually
//! happened (final names after conflicts, items really trashed…).
//!
//! Undoing runs `inverse` as a new job; once it succeeds the record moves to
//! the redo stack. Redoing runs `forward` again, which yields a fresh record
//! (names may differ the second time) that goes back on the undo stack.
//! Permanent deletion is never recorded.

use std::cell::{Cell, OnceCell, RefCell};

use gettextrs::gettext;
use gtk::{glib, prelude::*, subclass::prelude::*};

use crate::services::file_ops::{OpKind, Operation};

/// Oldest records are dropped beyond this.
const MAX_RECORDS: usize = 100;

#[derive(Clone, Debug)]
pub struct UndoRecord {
    pub forward: Operation,
    pub inverse: Operation,
}

impl UndoRecord {
    pub fn new(forward: Operation, inverse: Operation) -> Self {
        Self { forward, inverse }
    }

    /// Short name of the action, for "Undo …" labels.
    pub fn label(&self) -> String {
        match self.forward.kind() {
            OpKind::Copy => gettext("Copy"),
            OpKind::Move => gettext("Move"),
            OpKind::Link => gettext("Create Link"),
            OpKind::Duplicate => gettext("Duplicate"),
            OpKind::Trash => gettext("Move to Trash"),
            OpKind::Restore => gettext("Restore"),
            OpKind::Rename | OpKind::BatchRename => gettext("Rename"),
            OpKind::Extract => gettext("Extract"),
            OpKind::Compress => gettext("Compress"),
            OpKind::CreateFolder => gettext("New Folder"),
            OpKind::CreateFile => gettext("New File"),
            OpKind::Delete | OpKind::EmptyTrash => String::new(),
        }
    }
}

/// The two stacks, without any GTK.
#[derive(Default, Debug)]
pub struct UndoStack {
    undo: Vec<UndoRecord>,
    redo: Vec<UndoRecord>,
}

impl UndoStack {
    /// A new user action: it can be undone, and the redo history is gone.
    pub fn record(&mut self, record: UndoRecord) {
        self.redo.clear();
        push_bounded(&mut self.undo, record);
    }

    pub fn pop_undo(&mut self) -> Option<UndoRecord> {
        self.undo.pop()
    }

    pub fn pop_redo(&mut self) -> Option<UndoRecord> {
        self.redo.pop()
    }

    /// `record` was undone: it can now be redone.
    pub fn push_undone(&mut self, record: UndoRecord) {
        push_bounded(&mut self.redo, record);
    }

    /// An action was redone: it can be undone again, keeping the rest of the
    /// redo history.
    pub fn push_redone(&mut self, record: UndoRecord) {
        push_bounded(&mut self.undo, record);
    }

    /// An undo/redo couldn't run: put the record back where it was.
    pub fn restore_undo(&mut self, record: UndoRecord) {
        push_bounded(&mut self.undo, record);
    }

    pub fn restore_redo(&mut self, record: UndoRecord) {
        push_bounded(&mut self.redo, record);
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo_label(&self) -> Option<String> {
        self.undo.last().map(UndoRecord::label)
    }

    pub fn redo_label(&self) -> Option<String> {
        self.redo.last().map(UndoRecord::label)
    }
}

fn push_bounded(stack: &mut Vec<UndoRecord>, record: UndoRecord) {
    stack.push(record);
    if stack.len() > MAX_RECORDS {
        stack.remove(0);
    }
}

mod imp {
    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::UndoManager)]
    pub struct UndoManager {
        pub(super) stack: RefCell<UndoStack>,
        #[property(get)]
        pub(super) can_undo: Cell<bool>,
        #[property(get)]
        pub(super) can_redo: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for UndoManager {
        const NAME: &'static str = "LiaUndoManager";
        type Type = super::UndoManager;
    }

    #[glib::derived_properties]
    impl ObjectImpl for UndoManager {}
}

glib::wrapper! {
    /// The application-wide undo history (shared by all windows, like the
    /// file system it acts on).
    pub struct UndoManager(ObjectSubclass<imp::UndoManager>);
}

thread_local! {
    static MANAGER: OnceCell<UndoManager> = const { OnceCell::new() };
}

/// The application's undo manager.
pub fn undo_manager() -> UndoManager {
    MANAGER.with(|cell| cell.get_or_init(glib::Object::new).clone())
}

impl UndoManager {
    /// Runs `f` on the stacks, then updates the properties.
    pub fn with<R>(&self, f: impl FnOnce(&mut UndoStack) -> R) -> R {
        let imp = self.imp();
        let result = f(&mut imp.stack.borrow_mut());
        let (can_undo, can_redo) = {
            let stack = imp.stack.borrow();
            (stack.can_undo(), stack.can_redo())
        };
        if imp.can_undo.replace(can_undo) != can_undo {
            self.notify_can_undo();
        }
        if imp.can_redo.replace(can_redo) != can_redo {
            self.notify_can_redo();
        }
        result
    }

    pub fn undo_label(&self) -> Option<String> {
        self.imp().stack.borrow().undo_label()
    }

    pub fn redo_label(&self) -> Option<String> {
        self.imp().stack.borrow().redo_label()
    }
}
