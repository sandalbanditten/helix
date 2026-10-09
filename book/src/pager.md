# Pager

`hx -p` (`hx --pager`) shows text to read, like `less -R`. Piped text and the files given open
read-only, with the formatting of terminal output shown as a terminal shows it: colors, and bold
and underlined text. Every file opened later in the session opens the same way.

| Command | Shows |
| --- | --- |
| `cmd \| hx -p` | The output of `cmd` |
| `hx -p <files>` | The files |

The text can be searched, selected and yanked as in any buffer, but edits and `:w` are refused.
`:q` quits.

To read man pages and the output of git in Helix:

```sh
export MANPAGER='hx -p'
git config --global core.pager 'hx -p'
```

Man pages, which start with their title at both ends of the first line, get colors for their
parts too, like `bat` gives them:

| Part | Theme key | Falls back to |
| --- | --- | --- |
| Title and footer | `man.title` | `markup.heading` |
| Headings | `man.heading` | `markup.heading` |
| Options, like `-a` and `--all` | `man.option` | `constant` |
| Arguments, the text man underlines or italicizes | `man.argument` | `variable.parameter` |
| References, like `stat(2)` | `man.reference` | `function` |
| The section of a reference | `man.section` | `constant.numeric` |
| Links | `man.link` | `markup.link.url` |
| Environment variables, like `$HOME` | `man.variable` | `variable.builtin` |

A man page that `man` shows in `hx -p` is formatted again to fit the text of the view, the way
Neovim's `:Man` does: when it opens, and whenever the view gets wider or narrower. `MANWIDTH`, if
set, is the widest it gets. This needs `man-db`, which tells the pager the page it shows.
