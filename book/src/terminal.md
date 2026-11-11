# Terminal

`:term` opens a terminal: a buffer running the shell, or a command, on a pseudo-terminal. It works
like Neovim's `:terminal`. In insert mode keys go to the program; `Esc` goes to normal mode, where
the text, the screen at its end and what scrolled away above it, can be searched, selected and
yanked like any buffer, but not edited. `i`, `a`, `I` and `A` go back to the program at its cursor.

| Command | Opens |
| --- | --- |
| `:term [command]` | The shell, or `command` run by `shell`, in the current split |
| `:vterm [command]` | The same in a vertical split |
| `:hterm [command]` | The same in a horizontal split |
| `Space t` | The terminal used last, shown beside the current split if hidden, or a new one |

`Ctrl-\` sends the key after it as it is, so `Ctrl-\ Esc` sends `Esc` to the program, like to a
Helix running in the terminal.

The program sees a terminal the size of the view, a column narrower than its text, which follows
splits and resizes. Full-screen programs like `btop`, `less` and `tmux` draw on the screen while
the text above it stays. When the program ends, the buffer stays with how it ended, and `i` runs it
again. The last 10 000 lines above the screen are kept, and `clear` clears them.

The shell is `$SHELL`, else `sh`, unless set:

```toml
[editor]
terminal-shell = ["fish"]
```
