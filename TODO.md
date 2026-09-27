# TODO

Remember to be idiomatic, focus on clean code and architecture.
Follow design principles like loose coupling and high cohesion.
Read the files in `docs/`, especially `architecture.md` and `vision.md` and do things the rust and helix way.
Take your time and ask about specification details rather than guessing.
Look online for examples of similar features and implementations.
Ask when in doubt about specification or implementation, don't guess.
Performance is very important, especially for large files, directories, and projects.

<!-- MAYBE -->
## Emacs' `dired` Style Feature
Possibly a `dired` style view in the editor.
Consider how it should integrate with the file-tree.
Either zero integration, i.e. it opens in a buffer unrelated to the file-tree.
Maybe full integration, where the filetree _is_ a `dired` buffer.

<!-- MAYBE -->
## File watching with `notify` crate
