# TODO

Remember to be idiomatic, focus on clean code and architecture.
Follow design principles like loose coupling and high cohesion.
Read the files in `docs/`, especially `architecture.md` and `vision.md` and do things the rust and helix way.
Take your time and ask about specification details rather than guessing.
Look online for examples of similar features and implementations.
Ask when in doubt about specification or implementation, don't guess.
Performance is very important, especially for large files, directories, and projects.

### Emacs' `dired` Style Feature
We want a `dired` style view in the editor.
I.e. hitting a keybind on the filetree fullscreens it and shows the same output as `eza --git -aolg`.
Another keybind for just the currently hovered directory, or item in said directory.
When the filetree expands to this view, user should be able to edit the buffer, and changes to permission bits, user/group ownership, filename.
This includes things like `file.md` -> `./dir-that-may-or-may-not-exist/file.md`, which should use helix' `:w!` semantic for creating the directory, like using `:o` to create a new directory.
Additionally `file.md` to `../file.md` should work.
Access time stamps should also be editable.
Git markers should also be editable, e.g. `-I` to `--` should remove the file from `.gitignore` and `-M` to `--` should restore and `M-` to `--` should unstage.

## File Tree
We want a copy keybind (`y`) which copies a file and a (`p`) to paste it, or similar interface.

There is a bug where sometimes when opening popups like `space k` for documentation; the popups show up where they should be.
If the file tree _wasn't_ there, e.g. opening a popup on the first column of the editor viewport shows the popup on the first column on the terminal.

There is also a bug which causes cursor flickering, which I believe was introduced with the file tree feature.
This should be fixed.

<!-- MAYBE -->
## File watching with `notify` crate
Configurable to either:
- Popup saying file changed, if it already had unwritten changes
- Just reload the changes
