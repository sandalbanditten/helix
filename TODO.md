# TODO

Remember to be idiomatic, focus on clean code and architecture.
Follow design principles like loose coupling and high cohesion.
Read the files in `docs/`, especially `architecture.md` and `vision.md` and do things the rust and helix way.
Take your time and ask about specification details rather than guessing.
Look online for examples of similar features and implementations.
Ask when in doubt about specification or implementation, don't guess.
Performance is very important, especially for large files, directories, and projects.

## Diff view
Structural diff view using `difft` to use as `git difftool` or to use like `hx --diff file_a file_b` (or `-d` instead).
Should show both old and new in side-by-side buffers, with synchronized scrolling, i.e. just sync line, not column, like nvimdiff's `:scrollbind`.
Should syntax highlight the code, also show unchanged code.
Should also be callable for current buffer in helix, which should open it and its currently committed version in the view, same as calling `git diff file_a` when it has unstaged changes.
In this view `]g`, `[g` and their upper case friends should jump hunks.
Much like `difftastic.nvim`, with changes highlighting the line to coloring the _background_ red or green, and actually changed text a slightly brighter red or green, and "empty" lines (present in one but not the other) given a gray background.
When using it as a difftool, then on the left should be a docked, repurposed file tree with just the diff'd files, exactly like the file tree, but with a trailing `+nnn -mmm` with lines added/removed in green/red.

See `difftastic.nvim/assets/header.png`.

After it is implemented the undo-tree diff should be updated to use the code for this diff view, so that it has syntax highlighting.
