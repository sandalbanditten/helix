# Dired

Dired lists a directory in a buffer, and writing the buffer changes the files to match its edited
lines. `:dired` lists the current buffer's directory, or the one given, and `e` and `E` in the
[file tree](./file-tree.md) list the directory under the cursor or the whole tree.

```
0644 .rw-r--r--  4 user user  1 Oct 10:59 -M ├──  notes.txt
```

Every column but the size can be edited:

| Column | Editing it |
| --- | --- |
| Permissions | Changes the mode |
| User, group | Changes the owner or the group |
| Date | Changes the modification time |
| Git status | Stages, unstages, discards or ignores changes, see below |
| Name | Renames or moves the entry, and deleting it deletes the entry |
| Link target | Points the link elsewhere |

`:w` applies the edits, or none of them when one has a problem, which is then shown as a
diagnostic. Edits that delete entries or discard changes are applied by `:w!`. Deleting a line
deletes its entry, and pasting a yanked line copies its entry. `:reload` lists the directory again,
dropping the edits.

## Git

The git column holds the staged and the unstaged status of an entry. Editing them changes the
entry's status:

| Edit | Does |
| --- | --- |
| `-M` to `M-` | Stages the change |
| `M-` to `--` | Unstages the change |
| `-M` to `--` | Discards the change, with `:w!` |
| `-N` to `-I` | Ignores the file |
| `-I` to `--` | Stops ignoring the file |

Changing the git status needs a [trusted workspace](./workspace-trust.md).
