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

| Part | Theme scope |
| --- | --- |
| Title, footer and headings | `markup.heading` |
| Options, like `-a` and `--all` | `constant` |
| Arguments, the text man underlines or italicizes | `variable.parameter` |
| References, like `stat(2)` | `function`, and `constant.numeric` for the section |
| Links | `markup.link.url` |
| Environment variables, like `$HOME` | `variable.builtin` |

Man pages are formatted to the width of the terminal. Set `MANWIDTH` to a smaller width, like `80`,
to fit them beside the gutter.
