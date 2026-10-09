//! The man pages of the pager, which `man` formats again for the width of the views showing them.

use std::{collections::HashMap, process::Stdio};

use helix_view::{pager::ManPage, DocumentId, Editor};

use crate::job;

/// Has `man` format the man pages shown in the pager again where the width of their text
/// changed: the narrowest of the views showing a page, and at most `MANWIDTH`, like Neovim's
/// `:Man`. Each page waits for the formatting under way.
pub fn fit_man_pages(editor: &mut Editor) {
    let mut widths: HashMap<DocumentId, u16> = HashMap::new();
    for (view, _) in editor.tree.views() {
        let doc = &editor.documents[&view.doc];
        if man_page(editor, doc.id()).is_some() {
            let width = view.inner_width(doc);
            widths
                .entry(doc.id())
                .and_modify(|narrowest| *narrowest = (*narrowest).min(width))
                .or_insert(width);
        }
    }
    let limit = std::env::var("MANWIDTH")
        .ok()
        .and_then(|width| width.parse::<u16>().ok());
    for (doc, width) in widths {
        // A line as wide as the view would wrap before its line break.
        let width = width.saturating_sub(1);
        let width = limit.map_or(width, |limit| width.min(limit));
        let Some(page) = man_page_mut(editor, doc) else {
            continue;
        };
        if page.width == Some(width) || page.formatting.is_some() {
            continue;
        }
        page.formatting = Some(width);
        let args = page.args();
        job::in_background(
            move || {
                // `MANPAGER=cat` keeps `man` from starting the pager again, and
                // `MAN_KEEP_FORMATTING` keeps its overstrikes in output to a pipe.
                std::process::Command::new("man")
                    .args(&args)
                    .env("MANPAGER", "cat")
                    .env("MANWIDTH", width.to_string())
                    .env("MAN_KEEP_FORMATTING", "1")
                    .stdin(Stdio::null())
                    .stderr(Stdio::null())
                    .output()
            },
            move |editor, _, output| {
                let page = output
                    .ok()
                    .filter(|output| output.status.success() && !output.stdout.is_empty());
                if let Some(page) = page {
                    editor.repage(doc, &String::from_utf8_lossy(&page.stdout));
                }
                // Not again for this width, even if `man` failed.
                if let Some(page) = man_page_mut(editor, doc) {
                    page.formatting = None;
                    page.width = Some(width);
                }
            },
        );
    }
}

fn man_page(editor: &Editor, doc: DocumentId) -> Option<&ManPage> {
    editor.document(doc)?.page.as_ref()?.man_page.as_ref()
}

fn man_page_mut(editor: &mut Editor, doc: DocumentId) -> Option<&mut ManPage> {
    editor.document_mut(doc)?.page.as_mut()?.man_page.as_mut()
}
