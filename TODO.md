# TODO

Remember to be idiomatic, focus on clean code and architecture.
Follow design principles like loose coupling and high cohesion.
Read the files in `docs/`, especially `architecture.md` and `vision.md` and do things the rust and helix way.
Take your time and ask about specification details rather than guessing.
Look online for examples of similar features and implementations.
Ask when in doubt about specification or implementation, don't guess.
Performance is very important, especially for large files, directories, and projects.

## File Tree
We want a copy keybind (`y`) which copies a file and a (`p`) to paste it, giving it a name like `src/file.rs` -> `src/file-1.rs`, or a similar interface.
Should feel similar to the `dired` feature to use.
This keybind should also put the path relative to workspace root in system clipboard.

There is a bug where sometimes when opening popups like `space k` for documentation; the popups show up where they should be.
If the file tree _wasn't_ there, e.g. opening a popup on the first column of the editor viewport shows the popup on the first column on the terminal.
An example is running `! gradle test` in a java workspace.

There is also a bug which causes cursor flickering, which I believe was introduced with the file tree feature.
The flickering may happen even when there is no file tree open, and it usually happens in insert mode
This should be fixed.

MAYBE
## Compile command
Should make a pop-up sized like `space e` and similar, or a new buffer, give pros and cons for each, including performance and implementation complexity.
In said view a language-specific compile command (configured in `languages.toml`) should be run, e.g. `cargo build` or `gradle build`.
Should have another command for running tests e.g. `gradle test`.
Should turn file positions (e.g. `src/lib.rs:7:5` for rust) in compilation error into "hyperlinks" where `g f` and/or `g d` open the file at the location in a new buffer.
Overall it should work much like Emacs' Compilation Mode.
In this view the keybind `]d` etc. should go to next hyperlink to the file position (_locus_ in Emacs terminology).

MAYBE
## File watching with `notify` crate
Popup saying file changed, like there already already exists, if it already had unwritten changes.
Otherwise just reload the changes
