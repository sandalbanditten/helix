# Compilation

`:compile` runs the build command of the current buffer's language, like `cargo build`, and shows
its output in the compilation buffer, over the whole editor, as it arrives. The file positions in
the output, like `src/lib.rs:3:9`, are its _loci_: `]q` and `[q` select them one by one, from any
buffer, and `gf` opens the one under the cursor.

## Running commands

| Command | Runs |
| --- | --- |
| `:compile` | The `compile-command` of the buffer's language, like `cargo build` |
| `:compile-test` | Its `test-command`, like `cargo test` |
| `:compile-any <command>` | Any shell command, like `cargo check` |
| `:compile-kill` | Stops the running command and keeps its output: with `SIGTERM`, then with `SIGKILL` what still runs a second later |

While file buffers have unsaved changes, which a build would miss, these commands refuse to run
and name the buffers; `:compile!`, `:compile-test!` and `:compile-any!` run anyway.

Commands run through [`editor.shell`](./editor.md#editor-section), like `:sh`, without input. They
run in the directory the buffer's language server gets: the topmost one in the workspace with one
of the language's [root markers](./languages.md#project-and-lsp-root-selection), like
`Cargo.toml`, else the workspace root. A buffer without a language uses the workspace root.

In the compilation buffer, `:compile` and `:compile-test` run the commands of the language of the
last run, in its directory, and `:reload` runs the last command again.

The commands are configured per language in `languages.toml`, and can use
[expansions](./command-line.md#expansions):

```toml
[[language]]
name = "rust"
compile-command = "cargo clippy"
test-command = "cargo nextest run"

[[language]]
name = "typst"
compile-command = "typst compile %{file_path_absolute}"
```

Helix comes with these:

| Languages | `compile-command` | `test-command` |
| --- | --- | --- |
| `rust` | `cargo build` | `cargo test` |
| `go` | `go build ./...` | `go test ./...` |
| `zig` | `zig build` | `zig build test` |
| `typst` | `typst compile %{file_path_absolute}` | |
| `java`, `kotlin` | `gradle build` | `gradle test` |
| `scala` | `sbt compile` | `sbt test` |
| `haskell` | `cabal build` | `cabal test` |
| `c`, `cpp` | `make` | `make test` |

A workspace's own `.helix/languages.toml` is only read in a
[trusted workspace](./workspace-trust.md), so an untrusted project cannot set the commands that
run.

## The compilation buffer

There is one compilation buffer, named after its command, like `[compilation] cargo build`. It
covers the editor, the file tree too unless [`hide-file-tree = false`](./editor.md#editorcompilation-section).
A new run replaces its output and stops the run before it. Closing the buffer, or quitting Helix, stops
its run too; closing its split leaves the buffer, and its run, in the background. When the command
exits, what it left running in the background is stopped as well, so that the run ends; output
that arrives later than two seconds after is left out.

The output follows the command and the directory and time it started in, and ends with how it
ended, like `Exited with code 101 at 14:03:15 after 2.81 s`, which the statusline shows too. With
the cursor on the last line, the cursor stays on it as output arrives; elsewhere, it stays put.

On Unix, commands write to a terminal, as in a shell, so they color their output, show their
progress and lay it out as they do in a shell: `rg` puts the file of its matches on a line of its
own, unless told `--no-heading`, say. The terminal is as large as the split showing the output,
and commands are told when that changes. It is an `xterm-256color` (`TERM`) without input:
commands that ask for some, like `sudo` for a password, fail instead of waiting, and the pagers
are `cat` (`PAGER`, `GIT_PAGER` and `MANPAGER`).

The last rows of the output are the screen of the terminal: what commands draw there, like the
progress bars of `cargo build` and `cargo nextest run`, shows as they redraw it, and rows that
scroll off the screen stay as they were. A line wider than the terminal is one line again, unless
its rows are filled up with spaces to their end, as some commands write lines. Full-screen
programs, like `htop`, don't work there, and what they draw goes when they end.

Where Helix has no terminal to give them, as on Windows, commands write to a pipe instead, which
most tools don't color, so they are asked for colors with `CARGO_TERM_COLOR=always`,
`CLICOLOR_FORCE=1` and `FORCE_COLOR=1`, unless these are set already. Others take a flag, like
Typst's `--color always`.

The output keeps its colors, in the colors of the terminal Helix runs in, unless
[`colors = false`](./editor.md#editorcompilation-section) leaves them out.

The buffer can be edited, but not written, and never counts as modified: it closes and quits
without asking. Undo doesn't go back past the output that arrived.

## Loci

A file position in the output is a locus when its file exists:

| Output | Like |
| --- | --- |
| `path:line:column` and `path:line` | `src/lib.rs:3:9`, `main.cpp:4:24`, `file:///…/Main.kt:12:5` |
| `path(line,column)` | `a.ts(1,5)` |
| `path:[line,column]` | `App.java:[4,13]` |
| `path:(line,column)-(line,column)` | `src/Main.hs:(12,5)-(14,10)` |
| `File "path", line line` | `File "app.py", line 4` |

Relative paths are looked up in the directory the command ran in, then in its parent directories
up to the workspace root. A bare file name that isn't found there, like `AppTest.java:6` from a
Gradle test, is looked up by name below that directory (in its first 50,000 files, ignoring what
the file picker ignores); the package of a stack frame, like `demo` in
`at demo.App.main(App.java:5)`, picks among several files of that name.

Loci are the diagnostics of the compilation buffer: they are underlined and marked in the gutter,
the statusline counts them, `]d`, `[d`, `]D` and `[D` move between them, and `Space d` lists them.
A locus is an error, a warning, a note or a hint as its message says: the word after it
(`main.c:3:7: warning: …`), a word before it (`[error] …`, `e: …`, `… panicked at …`, an
exception like `AssertionFailedError at AppTest.java:6`), or the line above it
(`error[E0425]: …`); else it is a note.

| Key | Description |
| --- | --- |
| `gf`, `gd` | In the compilation buffer, open the locus on the cursor's line back in your layout: in a split already showing the file, else in the split the buffer covers the editor from, as it has none of its own. With [`open = "beside"`](./editor.md#editorcompilation-section), beside the buffer instead, in the split the command was run from, else in another split, else in a new one; with `open = "replace"`, in the buffer's own split |
| `]q`, `[q` | Select the next or previous locus in the compilation buffer, which gets the focus. A hidden buffer shows where `gf` would open a file, and goes on from the locus selected or opened last |

Columns are counted in characters, as with `:open file:7:5`, except on lines with tabs when the
compiler counts display columns, tabs to the next multiple of eight, as gcc and GHC do. That shows
in the caret of the source excerpt they print below the message, or in a column past the end of
the line.
