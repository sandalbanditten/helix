# Compilation

`:compile` runs the build command of the current buffer's language, like `cargo build`, and shows
its output in the compilation buffer, over the whole editor, as it arrives. The file positions in
the output, like `src/lib.rs:3:9`, are its _loci_: `gf` opens the one under the cursor beside the
output, and `]q` and `[q` visit them one by one from any buffer.

## Running commands

| Command | Runs |
| --- | --- |
| `:compile` | The `compile-command` of the buffer's language, like `cargo build` |
| `:compile-test` | Its `test-command`, like `cargo test` |
| `:compile-any <command>` | Any shell command, like `cargo check` |
| `:compile-kill` | Stops the running command and keeps its output |

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

There is one compilation buffer, named after its command, like `[compilation] cargo build`. A new
run replaces its output and stops the run before it. Closing the buffer, or quitting Helix, stops
its run too; closing its split leaves the buffer, and its run, in the background.

The output follows the command and the directory and time it started in, and ends with how it
ended, like `Exited with code 101 at 14:03:15 after 2.81 s`, which the statusline shows too. A line
shows once it ends. Colors and other escape sequences are left out, and a carriage return lets the
rest of a line overwrite it, as in a terminal. With the cursor on the last line, the cursor stays
on it as output arrives; elsewhere, it stays put.

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
(`main.c:3:7: warning: …`), a word before it (`[error] …`, `e: …`, `… panicked at …`), or the
line above it (`error[E0425]: …`); else it is a note.

| Key | Description |
| --- | --- |
| `gf`, `gd` | In the compilation buffer, open the locus on the cursor's line beside the buffer: in the split the command was run from, else in another split, else in a new one |
| `]q`, `[q` | Open the next or previous locus, after the one opened last, or the cursor in the compilation buffer, and move the buffer's cursor to it |

Columns are counted in characters, as with `:open file:7:5`. gcc and GHC count tabs to the next
multiple of eight, so on lines indented with tabs their positions land further to the right.
