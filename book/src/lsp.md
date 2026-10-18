# Language servers

Helix has built-in support for the [Language Server Protocol][lsp], providing
IDE-like features — diagnostics, completion, goto-definition, rename and more,
for any language that has a server configured. Language servers are separate
programs you install yourself: the [language support](./lang-support.md) page
lists which languages ship with a server configured, and the
[language server configuration wiki][wiki] has per-server installation notes.

Run `hx --health <language>` to check whether Helix found the configured server
for a language. After changing a server's configuration, `:lsp-restart` reloads
it and `:lsp-stop` stops it; `:lsp-workspace-command` runs a command the server
exposes for the workspace.

## Features

Most features are bound by default (see the [keymap](./keymap.md)); the relevant
keys are listed below.

| Feature | Default key / command |
| --- | --- |
| Diagnostics — shown inline and in the gutter | navigate with `[d` / `]d`; `Space-d` / `Space-D` open the document / workspace pickers |
| Completion | automatic while typing; `Ctrl-x` triggers it manually |
| Hover documentation | `Space-k` |
| Signature help (parameter hints) | automatic while typing arguments |
| Goto definition / declaration / type / references / implementation | `gd` / `gD` / `gy` / `gr` / `gi` |
| Rename symbol | `Space-r` |
| Code actions | `Space-a` |
| Document / workspace symbols | `Space-s` / `Space-S` |
| Format document | `:format`, or set `auto-format` to format on save |
| Inlay hints | enable with `display-inlay-hints` (see below) |

## Configuration

The [`[editor.lsp]`](./editor.md#editorlsp-section) section of `config.toml`
toggles LSP behaviour — `display-inlay-hints`, `auto-signature-help`,
`auto-document-highlight`, `snippets`, and `enable` to disable LSP entirely. How
much diagnostic detail is shown inline at the cursor is configured separately in
[`[editor.inline-diagnostics]`](./editor.md#editorinline-diagnostics-section).

Which server(s) a language uses, the server's command and arguments, and any
language-specific settings are configured in `languages.toml` (see
[Language Server configuration](./languages.md#language-server-configuration)). A
language may use [several servers](./languages.md#configuring-language-servers-for-a-language):
for each request the first server in the list that supports it is used, and a
server's features can be limited with `only-features` / `except-features`.

## Following the view

Servers that set the experimental capability `helixViewport` are sent the
notification `helix/viewport` whenever the focused view's document, first line
or cursor changes, so that previews can follow the editor:

| Field | Description |
| --- | --- |
| `uri` | The focused view's document |
| `topLine` | The first line the view shows, counting from 0 |
| `cursor` | The position of the primary cursor |
| `scrollDuration` | How long the view takes to scroll, in milliseconds: the [`smooth-scroll`](./editor.md#editorsmooth-scroll-section) duration, or 0 when it is disabled |

[lsp]: https://microsoft.github.io/language-server-protocol/
[wiki]: https://github.com/helix-editor/helix/wiki/Language-Server-Configurations
