# TODO

Remember to be idiomatic, focus on clean code and architecture.
Follow design principles like loose coupling and high cohesion.
Read the files in `docs/`, especially `architecture.md` and `vision.md` and do things the rust and helix way.
Take your time and ask about specification details rather than guessing.
Look online for examples of similar features and implementations.
Ask when in doubt about specification or implementation, don't guess.
Performance is very important, especially for large files, directories, and projects.

## Undo Tree
We want a vim-style undo tree instead of linear undo.
It should be configurable to be persistent, like vim can.
It have a similar interface to vims and emacs, but be as helix-like as possible.

## Diff view
Structural diff view using `difft` to use as `git difftool`.
Should show both old and new in side-by-side buffers, with synchronized scrolling, i.e. just sync line, not column.
Should syntax highlight the code, also show unchanged code.
Should also be callable for current buffer in helix.
In this view `]g`, `[g` and their upper case friends should jump hunks.
Much like `difftastic.nvim`, with changes highlighting line the _background_ red or green, actually changes text a slightly brighter red or green, and "empty" lines (present in one but not the other) given a gray background.
On the left should be a docked, repurposed file tree with just the diff'd files, exactly like the file tree, but with a trailing `+nnn -mmm` with additions in green and removals in red.
See `difftastic.nvim/assets/header.png`.
