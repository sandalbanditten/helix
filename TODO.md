# TODO

Remember to be idiomatic, focus on clean code and architecture.
Follow design principles like loose coupling and high cohesion.
Read the files in `docs/`, especially `architecture.md` and `vision.md` and do things the rust and helix way.
Take your time and ask about specification details rather than guessing.
Look online for examples of similar features and implementations.
Ask when in doubt about specification or implementation, don't guess.
Performance is very important, especially for large files, directories, and projects.

MAYBE
## Compile command
Should make a new fullscreen buffer, like `dired`.
In said buffer a language-specific compile command (configured in `languages.toml`) should be run, e.g. `cargo build` or `gradle build`.
Should have another command for running tests e.g. `gradle test`.
Should turn file positions (e.g. `src/lib.rs:7:5` for rust) in compilation error into "hyperlinks" where `g f` and/or `g d` open the file at the location in a new buffer.
Overall it should work much like Emacs' Compilation Mode.
In this view the keybind `]d` etc. should go to next hyperlink to the file position (_locus_ in Emacs terminology).

MAYBE
## File watching with `notify` crate
Popup saying file changed, like there already already exists, if it already had unwritten changes.
Otherwise just reload the changes
