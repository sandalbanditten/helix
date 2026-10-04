# Diff view

The diff view shows two versions of a file side by side, lined up by
[difftastic](https://difftastic.wilfred.me.uk/)'s structural diff: the old version on the left
with its removed lines on red, the new one on the right with its added lines on green, and the
text that changed within a line brighter. Gray filler rows face the lines the other side lacks,
so lines that correspond sit on the same row. Both sides are highlighted by their syntax and shown
whole, unchanged code included.

The two panes cover the editor and scroll together, line by line, like Vim's `scrollbind`; each
keeps its own horizontal scroll. Moving the cursor in one pane moves the other's to the same row,
so switching panes with `Ctrl-w w` keeps your place. The panes are read-only and don't wrap. The
filler rows are not part of the text: line numbers, relative ones included, count only real lines.
`:q` in either pane closes both and brings back the layout from before.

## Opening a diff

| Command | Diffs |
| --- | --- |
| `:diff` | The buffer against its committed version, HEAD's, which the [diff gutter](./editor.md#editorguttersdiff-section) also shows |
| `:diff-changes [dir]` | Every file changed since HEAD in the working directory, or in `dir`: staged, unstaged and new ones |
| `hx -d`, `hx --diff` | Likewise, in the working directory |
| `hx -d <dir>` | Likewise, in `dir` |
| `hx -d <file>` | The file against its committed version |
| `hx -d <old> <new>` | Two files. `/dev/null` for either shows the other as created or deleted |
| `hx -d <old> <new> <path>` | Two files as versions of the file at `path`: named after it, in its language, and `gf` opens it. An empty `path` names nothing |
| `hx -d <old-dir> <new-dir>` | The files of two directories, paired by their paths within them. Identical files are left out, and so are the ones git ignores |

The diff follows a buffer diffed with `:diff`: when the buffer or its committed version changes,
the diff is worked out anew.

A diff opens once difftastic has lined the two versions up. That is quick for most changes, but
can take a few seconds for many changes in a big file. Meanwhile the status line says
`Diffing <file> with difftastic…`, and the editor stays usable. Where `difft` isn't installed, or
fails on a file, Helix lines the versions up with its own line diff, which highlights the words
that changed, and the status line says why. [`[editor.diff] tool`](./editor.md#editordiff-section)
picks that diff always.

## Moving through a diff

| Key | Description |
| --- | --- |
| `]g`, `[g` | Go to the next, previous hunk. In a diff of many files, past the last hunk goes to the next file's first, and before the first to the previous file's last |
| `]G`, `[G` | Go to the last, first hunk |
| `gf`, `Enter` | Open the file at the line under the cursor, in the diff's place |

A hunk is a run of rows that changed. Going to one selects its lines in the focused pane; where
the pane has only filler rows there, the cursor goes to the line after them. Counts work as
usual.

`gf` and `Enter` close the diff and open the file, the new version, in the split the diff was
opened from. From the old version's pane, the file opens at the line on the same row.

## Diffs of many files

When a diff has many files, the diff tree lists them on the left, or on the right with the file
tree's [`side`](./editor.md#editorfile-tree-section), in place of the file tree. Each file shows
the lines it adds and removes, like `+12 -3` in `git diff --stat`, and each directory the sums
of its files. The file shown is marked. The diffs of the files next to it are worked out ahead,
so `]g` and `[g` get there without waiting.

`Space e` focuses the diff tree on the file shown, and `Space E` hides or shows it. While it is
focused it takes these keys; any other key gives the editor its focus back and runs there.

| Key | Description |
| --- | --- |
| `j`, `Down` | Move down |
| `k`, `Up` | Move up |
| `l`, `Right` | Expand directory |
| `h`, `Left` | Collapse directory |
| `Ctrl-d`, `Ctrl-u` | Move half a page down, up |
| `PageDown`, `PageUp` | Move a page down, up |
| `gg`, `Home` | Go to the first row |
| `ge`, `End` | Go to the last row |
| `zz`, `zc` | Align the cursor row to the center |
| `zt`, `zb` | Align the cursor row to the top, bottom |
| `Enter` | Show the file's diff, or expand/collapse directory |
| `/` | Search for a file |
| `n`, `N` | Go to the next, previous match |
| `+`, `-` | Widen, narrow the diff tree |
| `=` | Fit the width to the widest row |
| `\|` | Toggle the widest and narrowest width |
| `?` | Show these keys |
| `Escape` | Return focus to the editor |

`:q` in a pane ends the diff, the diff tree with it. After `gf`, the diff tree stays, and `Enter`
on a file shows its diff again.

## Git

To make Helix git's diff tool, add this to git's config, `~/.config/git/config` or
`~/.gitconfig`:

```ini
[diff]
    tool = hx
[difftool]
    prompt = false
[difftool "hx"]
    cmd = hx --diff \"$LOCAL\" \"$REMOTE\" \"$MERGED\"
```

Git's config drops quotes that aren't escaped, so keep the `\"`;
`git config --global difftool.hx.cmd 'hx --diff "$LOCAL" "$REMOTE" "$MERGED"'` writes them.

`git difftool` then shows each changed file in a Helix of its own, one after the other. Git hands
over temporary copies, and `$MERGED` names the file: the panes are named like `src/main.rs (old)`
and `src/main.rs (new)`, and `gf` opens `src/main.rs` in the working tree. `git difftool -d`
(`--dir-diff`) shows all of them in one Helix, with the diff tree; its `$MERGED` is empty. Both
take what `git diff` takes, like `git difftool -d HEAD~3` or `git difftool -d main...`.

## Colors

The colors come from the theme's `diff.plus.line`, `diff.plus.text`, `diff.minus.line`,
`diff.minus.text` and `diff.filler` [scopes](./themes.md#interface). A theme without them gets
backgrounds blended from its `diff.plus`, `diff.minus` and `ui.text` over its background, as
difftastic.nvim does.
