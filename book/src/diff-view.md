# Diff view

The diff view shows two versions of a file side by side: the old one on the left, with its removed
lines in red, and the new one on the right, with its added lines in green. The text that changed
within a line is brighter. Lines that correspond are shown on the same row, with filler rows
facing the lines the other side lacks, and the two panes scroll together.

## Opening a diff

| Command | Diffs |
| --- | --- |
| `:diff` | The buffer against its committed version |
| `:diff-changes [dir]` | Every file changed since the last commit, in the working directory or in `dir` |
| `hx -d`, `hx --diff` | Every file changed since the last commit, in the working directory |
| `hx -d <dir>` | Every file changed since the last commit, in `dir` |
| `hx -d <file>` | The file against its committed version |
| `hx -d <old> <new>` | Two files, or two directories |
| `hx -d <old> <new> <path>` | Two files as versions of the file at `path` |

The diff of a buffer follows its changes. Diffs are lined up by
[difftastic](https://difftastic.wilfred.me.uk/) where `difft` is installed, and by Helix's own line
diff otherwise (see [`[editor.diff]`](./editor.md#editordiff-section)). The panes follow the
[soft wrap](./editor.md#editorsoft-wrap-section) settings.

## Moving through a diff

| Key | Description |
| --- | --- |
| `]g`, `[g` | Go to the next, previous hunk |
| `]G`, `[G` | Go to the last, first hunk |
| `gf`, `Enter` | Open the file at the line under the cursor |

`:q` in either pane closes the diff.

## Diffs of many files

When a diff has many files, the diff tree lists them in place of the file tree, with the lines each
adds and removes. A renamed file is listed once, below the directory its old and new paths share,
the way `git log --stat` shows it: `src/{ => ui}/main.rs`. `]g` and `[g` go on to the next or
previous file. `Space e` focuses the diff tree and `Space E` toggles it. While it is focused it
takes these keys; any other key returns focus to the editor.

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

## Git

To use Helix as git's diff tool, add this to git's config:

```ini
[diff]
    tool = hx
[difftool]
    prompt = false
[difftool "hx"]
    cmd = hx --diff \"$LOCAL\" \"$REMOTE\" \"$MERGED\"
```

`git difftool` then shows each changed file in turn, and `git difftool -d` shows all of them at once.

## Colors

The colors come from the `diff.plus.line`, `diff.plus.text`, `diff.minus.line`, `diff.minus.text`
and `diff.filler` [theme scopes](./themes.md#interface), and are otherwise blended from `diff.plus`,
`diff.minus` and `ui.text`.

To draw the filler rows with slashes:

```toml
[editor.diff]
filler-character = "╱"
```
