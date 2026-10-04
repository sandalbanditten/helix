# Compilation

`:compile` runs the build command of the current buffer's language and shows its output in the
compilation buffer as it arrives. The file positions in the output, like `src/lib.rs:3:9`, are its
_loci_: `]q` and `[q` go through them from any buffer, and `gf` opens the one under the cursor.

## Running commands

| Command | Runs |
| --- | --- |
| `:compile` | The `compile-command` of the buffer's language |
| `:compile-test` | The `test-command` of the buffer's language |
| `:compile-any <command>` | Any shell command |
| `:compile-kill` | Stops the running command |

The commands refuse to run while buffers have unsaved changes; `:compile!`, `:compile-test!` and
`:compile-any!` run anyway. A command runs in the
[root directory](./languages.md#project-and-lsp-root-selection) of the buffer's language, or else
in the workspace root. In the compilation buffer, `:reload` runs the last command again.

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

## The compilation buffer

The compilation buffer covers the editor and shows the output of the last command, in color, ending
with how it exited. A new command replaces the output, and closing the buffer stops its command.

The loci of the output are shown like diagnostics: underlined, marked in the gutter and listed by
`Space d`. Errors, warnings and notes are told apart by their messages.

| Key | Description |
| --- | --- |
| `gf`, `gd` | Open the locus on the cursor's line |
| `]q`, `[q` | Go to the next, previous locus |

Where files open is set by [`[editor.compilation]`](./editor.md#editorcompilation-section).
