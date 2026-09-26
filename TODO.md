# TODO

Remember to be idiomatic, focus on clean code and architecture.
Follow design principles like loose coupling and high cohesion.
Read the files in `docs/`, especially `architecture.md` and `vision.md` and do things the rust and helix way.
Take your time and ask about specification details rather than guessing.
Look online for examples of similar features and implementations.
Ask when in doubt about specification or implementation, don't guess.
Performance is very important, especially for large files, directories, and projects.

## Spellchecking
It should use `helix/spellbook` and work like `https://github.com/helix-editor/helix/pull/15910`.
Implement that PR again, but into this personal extended fork.
Make sure the implemented code is performant and idiomatic.
Remember to focus on code reuse and clean architecture.
Read through the design decisions by the original authors and consider them when planning/implementing.
Reuse as much as possible from the PR.

Consider these wanted features when making design and implementation decisions:
- Like `https://mitos.computer/docs/spell-checking`, tree-sitter decides what is checked (`spellcheck.scm` with `@spell`/`@nospell`).
  - Queries for Markdown, LaTeX and Typst, plus the PR's `comment` query (comments of languages that inject the comment grammar).
  - Code blocks inside prose documents are skipped entirely, including comments of their injected language.
  - Files without a syntax tree (plain text) are checked in full.
- Spell checking is opt-in like in the PR: `[editor.spelling] languages`, a language's `spelling.languages`, `.editorconfig` `spelling_language`, or `:set-spelling-language`.
- Danish spell checking must work: the tokenizer is not ASCII-only, and `da_DK` is bundled next to `en_US`.
- Saving re-checks the whole document, in addition to the incremental re-checks while typing.
- `[editor.spelling] messages = false` (default): findings get only the underline, `]s`/`[s` and code actions.
  - `messages = true` shows them like any hint diagnostic (inline/end-of-line messages, gutter, statusline, pickers, `]d`/`[d`).
- `]s`/`[s` go to the next/previous misspelling and behave like `]d`/`[d`.
- `space A` pops up the fixes for the misspelling under the cursor (suggestions and "Add to dictionary"), like `space a` without the language servers' actions.
- Misspellings are underlined with the `diagnostic.spelling` theme scope, which defaults to the theme's `diagnostic.error` style (red in most themes).

<!-- MAYBE -->
## Emacs' `dired` Style Feature
Possibly a `dired` style view in the editor.
Consider how it should integrate with the file-tree.
Either zero integration, i.e. it opens in a buffer unrelated to the file-tree.
Maybe full integration, where the filetree _is_ a `dired` buffer.

<!-- MAYBE -->
## File watching with `notify` crate
