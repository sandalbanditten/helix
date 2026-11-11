//! The pages of the pager that `man` and git format again for the width of the views showing them.

use std::collections::HashMap;

use helix_view::{
    pager::{self, Source},
    DocumentId, Editor,
};

use crate::job;

/// Has the commands that showed pages in the pager format them again where the width of their
/// text changed: the narrowest of the views showing a page, like Neovim's `:Man`, and at most the
/// width the command allows. Each page waits for the formatting under way.
pub fn fit_pages(editor: &mut Editor) {
    let mut widths: HashMap<DocumentId, u16> = HashMap::new();
    for (view, _) in editor.tree.views() {
        let doc = &editor.documents[&view.doc];
        if source(editor, doc.id()).is_some() {
            let width = view.inner_width(doc);
            widths
                .entry(doc.id())
                .and_modify(|narrowest| *narrowest = (*narrowest).min(width))
                .or_insert(width);
        }
    }
    for (doc, width) in widths {
        let Some(source) = source_mut(editor, doc) else {
            continue;
        };
        // A line as wide as the view would wrap before its line break.
        let width = width.saturating_sub(1);
        let width = source
            .producer
            .max_width()
            .map_or(width, |max| width.min(max));
        if source.width == Some(width) || source.formatting.is_some() {
            continue;
        }
        source.formatting = Some(width);
        let mut command = source.producer.command(width);
        job::in_background(
            move || {
                let output = command
                    .output()
                    .ok()
                    .filter(|output| output.status.success() && !output.stdout.is_empty())?;
                Some(pager::page(&String::from_utf8_lossy(&output.stdout)))
            },
            move |editor, _, page| {
                if let Some((text, page)) = page {
                    editor.repage(doc, text, page);
                }
                // Not again for this width, even if the command failed.
                if let Some(source) = source_mut(editor, doc) {
                    source.formatting = None;
                    source.width = Some(width);
                }
            },
        );
    }
}

fn source(editor: &Editor, doc: DocumentId) -> Option<&Source> {
    editor.document(doc)?.page.as_ref()?.source.as_ref()
}

fn source_mut(editor: &mut Editor, doc: DocumentId) -> Option<&mut Source> {
    editor.document_mut(doc)?.page.as_mut()?.source.as_mut()
}
