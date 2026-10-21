# helix-fork: custom changes and how to use them

Everything below is on `master`. The book (`book/src`) has a page or section for each feature.

## Display

| Feature | What it does | How to use it |
|---|---|---|
| **Smooth scrolling** | Animates view movements | `[editor.smooth-scroll] enable = true` (also `duration`, `hide-cursor`) |
| **Breadcrumbs** | Shows the functions, types and sections around the cursor in the statusline | On by default in the statusline's `left` list; options in `[editor.statusline.breadcrumbs]` |
| **Code folding** | Folds code using `fold.scm` queries | `zf` toggles a fold, `za` toggles it with the folds inside, `zF` folds everything, `zU` unfolds everything; `[editor.folding] start-folded`, `placeholder` |
| **Conceal** | Shows Typst symbols as glyphs (`arrow.r` → `→`). Hides the brackets and escapes of `[[wikilinks]]`. Draws `---` rules across the page | On by default; `[editor.conceal] enable`, `reveal = "adjacent" \| "symbol" \| "line"` |
| **Markdown rendering** | Draws Markdown rendered, like render-markdown.nvim: emphasis and code without their markers, links and images as an icon and their text, cycling bullets, task checkboxes, quote bars, callouts, code fences as their language's icon and rules, LaTeX symbols, and tables with aligned columns, box borders and wrapped cells. Headings stay as typed. An element shows as typed while the cursor is on or next to it. Also in doc comments and commit messages | On by default; `[editor.markdown] enable-render`, `icons`, `injected` |
| **Scrollbars** | Rails on splits that mark diagnostics and search matches; drag the thumb, or click the track to page | `[editor.scrollbar] enable = true` |
| **Minimap** | A braille map beside each split; click or drag it to scroll | `[editor.minimap] enable = true` (`width`, `lines-per-dot`) |
| **Bufferline scroll** | The bufferline scrolls, smoothly, to keep the active buffer in view | Automatic |

## Side panels

### File tree
- `Space e` focuses it on the current file and `Space E` toggles it. `hx .` opens with it shown.
- `j`/`k`/`h`/`l` move, `Enter` opens, and `Ctrl-s`/`Ctrl-v` open in a split.
- File operations:
  - `a` new file (a trailing `/` makes a directory), `A` new directory
  - `r` rename, `R` or `Ctrl-r` move
  - `d` delete
  - `y`, `x` and `p` copy, cut and paste
- `/` searches, `o` opens with the default application, `e` and `E` open dired, and `?` lists the keys.
- Rows show the git status (staged and unstaged apart), `*` when the file is open and `+` when it has unsaved changes.
- Options are in `[editor.file-tree]`: `side`, `icons`, `ls-colors`, `flatten-dirs` and more.

### Undo tree
- `Space u` focuses it and `Space U` toggles it. Moving through it takes the buffer to that revision: `Enter` keeps it and `Esc` goes back.
- `[` and `]` go by written revisions, `/` searches the changes, and `d` toggles the diff.
- `Alt-u`/`Alt-U` (`:earlier`/`:later`) go through revisions in time order, and `:earlier 2f` goes by file writes.
- `[editor.undo] persist = true` keeps undo history in undo files, which follow files when they're moved or copied. `float = true` floats the panel over the editor. `diff = "difftastic"` shows each revision's diff.

## Buffers and tools

### Dired
`:dired [dir]`, or `e` in the file tree, lists a directory as an editable buffer, and `:w` applies the edits:
- Editing a name renames or moves the entry, and deleting a line deletes it (applied with `:w!`).
- Pasting a yanked line copies the entry.
- You can also edit the mode, owner, group, modification time and link target.
- Editing the git column changes the git status: `-M`→`M-` stages, `M-`→`--` unstages, `-M`→`--` discards (with `:w!`) and `-N`→`-I` ignores.

### Compilation
- `:compile` and `:compile-test` run the language's `compile-command` and `test-command` from `languages.toml`.
- `:compile-any <cmd>` runs any command, `:compile-kill` stops it, and `:reload` in the output buffer reruns it.
- Commands run on a pty, so the output keeps its colours.
- File positions in the output (*loci*) become diagnostics: `]q`/`[q` go through them from any buffer, and `gf` opens the one under the cursor.
- Paths with spaces work, so `:compile-any rg --vimgrep foo` gives a clickable search list.
- Options are in `[editor.compilation]`: `open = "return" | "beside" | "replace"` and `hide-file-tree`.

### Diff view
Shows two versions side by side, aligned by difftastic when `difft` is installed:
- `:diff` diffs the buffer against HEAD, and `:diff-changes [dir]` diffs every changed file.
- From the shell: `hx -d`, `hx -d <file>`, `hx -d <old> <new>`. This also works as a git difftool.
- `]g`/`[g` jump between hunks and files, and `gf`/`Enter` open the file at that line.
- A diff of several files shows a diff tree (`Space e`/`Space E`), with renames listed like `git --stat`.
- Options are in `[editor.diff]`: `tool` and `filler-character`.

### Pager
`hx -p` works like `less -R`: `cmd | hx -p` or `hx -p <files>` open read-only, with the ANSI colours kept.
- Set `MANPAGER='hx -p'` and `git config --global core.pager 'hx -p'` to use it for man pages and git.
- Man pages get colours like `bat`, using the `man.*` theme keys, and reflow when the view's width changes.

### Auto-reload
- Buffers reload when their files change on disk. The reload is an undo step, so `u` reverts it.
- A buffer with unsaved changes asks first: `r` reloads, `k` keeps, and `R`/`K` do the same for all buffers asked about.
- The diff gutter and branch name follow HEAD.
- Turn it off with `auto-reload = false`.

## Prose

### Spell checking
- Turn it on per buffer with `:spelling en_US`, or per language, path or globally (`[editor.spelling] languages = [...]`).
- en_US and da_DK are bundled; you can add Hunspell dictionaries under `runtime/dictionaries/`.
- `]s`/`[s` go to the next or previous misspelling, `]S`/`[S` to the last or first.
- `Space A` offers corrections and "add to dictionary".
- `detect = true` checks each document against the language it's written in.
- Abbreviations with dots, like `e.g.`, are accepted.

## LSP and the Typst vault

- `Space a` shows only language-server code actions; spelling fixes are on `Space A`.
- **Viewport sync:** servers that set the experimental `helixViewport` capability get `helix/viewport` notifications with the focused document, its top line, the cursor and the scroll duration.
  - The `vault` branch of tinymist uses this to keep zathura scrolled to match Helix.
- In the vault, `gd`/`gf` on a `[[wikilink]]` open the note, `gr` lists backlinks, and `:move` updates links in other notes.
- Typst wikilinks are highlighted as links, and jump labels (`gw`) draw over link styles.

## Small tweaks

- `gf` opens `path:line:column` at that position.
- `:move` accepts a bare file name, which renames the file in its own directory.
- With a count, `j`/`k` move by lines rather than soft-wrapped rows, so `20k` matches the relative line numbers. Without a count they still move by rows.
- The cursor sits past inlay hints outside insert mode.
- Popups and completion docs are anchored to the editor area, beside the panels.
