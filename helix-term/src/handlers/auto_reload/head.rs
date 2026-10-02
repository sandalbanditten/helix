//! Refreshing what documents show of their repository once HEAD moved: their diff bases, which
//! are the files as of HEAD, and the name of HEAD.

use std::{collections::HashSet, path::PathBuf};

use helix_loader::workspace_trust::TrustQuery;
use helix_view::Editor;

use crate::job;

/// Reads the diff bases and HEAD names of the documents at `paths` anew in the background.
pub(super) fn refresh(paths: HashSet<PathBuf>, editor: &Editor) {
    let docs: Vec<_> = paths
        .into_iter()
        .filter_map(|path| {
            let doc = editor.document_by_path(&path)?;
            let trust_full = editor
                .workspace_trust
                .query(doc.workspace_root(), TrustQuery::Git)
                .is_trusted();
            Some((doc.id(), path, trust_full))
        })
        .collect();
    let registry = editor.diff_providers.clone();
    job::in_background(
        move || {
            docs.into_iter()
                .map(|(doc, path, trust_full)| {
                    let diff_base = registry.get_diff_base(&path, trust_full);
                    let head = registry.get_current_head_name(&path, trust_full);
                    (doc, diff_base, head)
                })
                .collect::<Vec<_>>()
        },
        |editor, _, refreshed| {
            for (doc, diff_base, head) in refreshed {
                if let Some(doc) = editor.document_mut(doc) {
                    doc.set_vcs(diff_base, head);
                }
            }
        },
    );
}
