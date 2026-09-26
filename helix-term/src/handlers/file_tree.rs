//! Keeps the file tree up to date with what happens in the editor.

use helix_event::register_hook;
use helix_view::{
    events::{DocumentDidSave, WorkingDirectoryDidChange},
    handlers::Handlers,
};

use crate::{job, ui};

pub(super) fn register_hooks(_handlers: &Handlers) {
    register_hook!(move |_event: &mut DocumentDidSave<'_>| {
        // Files in collapsed directories are not watched, but saving them changes their status.
        job::dispatch_blocking(|editor, compositor| {
            if let Some(editor_view) = compositor.find::<ui::EditorView>() {
                editor_view.file_tree.refresh_git(editor);
            }
        });
        Ok(())
    });

    register_hook!(move |_event: &mut WorkingDirectoryDidChange<'_>| {
        job::dispatch_blocking(|editor, compositor| {
            if let Some(editor_view) = compositor.find::<ui::EditorView>() {
                editor_view.file_tree.follow_working_directory(editor);
            }
        });
        Ok(())
    });
}
