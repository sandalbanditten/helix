# Spell checking

Helix can spell check documents with [Hunspell](https://hunspell.github.io/) dictionaries.
Misspellings are underlined, and their corrections and "Add to dictionary" are offered as code
actions.

Spell checking is opt-in. Tree-sitter decides which parts of a document are checked, such as the
prose of Markdown, LaTeX and Typst documents and the comments of code, so keywords and identifiers
aren't flagged. Several dictionaries can be used at once; a word is accepted when any of them
allows it.

## Enabling

Spell checking is turned on in one of these ways, from the highest precedence to the lowest:

1. Per buffer, with `:set-spelling-language` (or `:spelling`):

   ```
   :set-spelling-language en_US
   ```

   Pass several languages to check against all of them (`:spelling en_US da_DK`), or `off` to
   disable checking for the buffer. Without an argument it shows the current languages.

2. Per path, with the [`.editorconfig`](https://editorconfig.org/) `spelling_language` property,
   written `ss` or `ss-TT` as the EditorConfig specification describes:

   ```ini
   [*.md]
   spelling_language = en-US
   ```

3. Per language, in `languages.toml` (see [Languages](./languages.md)):

   ```toml
   [[language]]
   name = "markdown"
   spelling.languages = ["en_US"]
   ```

4. Globally, in `config.toml` (see [`[editor.spelling]`](./editor.md#editorspelling-section)):

   ```toml
   [editor.spelling]
   languages = ["en_US"]
   ```

## Detecting the language

With several languages configured, a word is accepted when any of their dictionaries knows it, so
a misspelled word that is a real word in another of the languages isn't flagged. Setting `detect`
checks each document with only the language it is written in instead:

```toml
[editor.spelling]
languages = ["en_US", "da_DK"]
detect = true
```

The language is detected from the checked text at the start of the document, among the configured
languages, when the document is opened or saved. When it can't be told reliably, as in a short
document, all the configured languages are used. A language chosen with `:set-spelling-language`
or `.editorconfig` is used as is. `:set-spelling-language` without an argument shows the languages
in use.

## Using

Misspellings are underlined with the `diagnostic.spelling` theme scope, which defaults to the
`diagnostic.error` style. By default they are only underlined: they have no message and are left
out of the gutter, the statusline, the diagnostics pickers and `]d`/`[d`. Set
[`messages = true`](./editor.md#editorspelling-section) to show them like other hint diagnostics.

| Key       | Description                                                                         |
| ---       | ---                                                                                 |
| `]s`      | Go to the next misspelling                                                          |
| `[s`      | Go to the previous misspelling                                                      |
| `]S`      | Go to the last misspelling                                                          |
| `[S`      | Go to the first misspelling                                                         |
| `Space A` | Fix the misspelling under the cursor: pick a suggestion, or add it to a dictionary  |
| `Space a` | Code actions, including the fixes of the misspelling under the cursor               |

While you type, the text around each edit is checked again after a second. Opening and saving a
document check all of it, which also catches text that an edit moved into or out of the checked
parts, such as a paragraph below a newly typed code fence.

## Scope

What is checked is decided per language by a `spellcheck.scm` query (see [Adding spellcheck
queries](./guides/spellcheck.md)):

| Language                    | Checked                                    | Skipped                                                                                          |
| ---                         | ---                                        | ---                                                                                              |
| Markdown                    | Prose, headings, lists, tables, link text  | Code blocks and spans, math, link destinations, autolinks, HTML, character references, front matter |
| LaTeX                       | Text, including section titles and captions | Commands, math, verbatim and code environments, environment names, options, labels, references, citations |
| Typst                       | Markup text                                | Code, raw text, math, escapes                                                                    |
| Languages with comments     | Comments                                   | Code                                                                                             |

Comments are checked in every language that injects the `comment` grammar, which most do. Code
blocks in Markdown are skipped entirely, including their comments. A document without a syntax
tree, like plain text, is checked in full. URLs and email addresses are never checked.

## Dictionaries

Dictionaries are Hunspell `.aff`/`.dic` pairs loaded from
`dictionaries/<language>/<language>.{aff,dic}` in the [runtime
directories](./building-from-source.md#configuring-helixs-runtime-files). Helix bundles `en_US`
and `da_DK`. To add another language, put its Hunspell files in a runtime directory, named after
the language used in the configuration. For example for `de_DE`:

```
~/.config/helix/runtime/dictionaries/de_DE/de_DE.aff
~/.config/helix/runtime/dictionaries/de_DE/de_DE.dic
```

Hunspell dictionaries for most languages are distributed with LibreOffice. The files are read in
the encoding their `.aff` file names with `SET`, and entries of the `.dic` file that can't be parsed
are skipped with a warning in the log. A dictionary that can't be loaded is reported in the
statusline.

### Personal dictionary

"Add to dictionary" accepts a word permanently by appending it to a personal dictionary, one word
per line:

```
<state>/dictionaries/<language>.txt
```

`<state>` is `~/.local/state/helix` on Linux, and Helix's data directory where there is no state
directory. Each language has its own file, so a word added for one language isn't accepted in
another. The file can be edited by hand; it is read when the dictionary is loaded.

## Tuning

Names, jargon and code-like tokens can be flagged. These keys, available globally under
`[editor.spelling]` and per language in `languages.toml`, reduce the noise:

| Key               | Description                                                              |
| ---               | ---                                                                      |
| `words`           | Extra accepted words, matched case-insensitively                         |
| `ignore-regexes`  | Tokens matching any of these are not checked (e.g. `"^[A-Z0-9_]+$"`)     |
| `min-word-length` | Tokens shorter than this are not checked                                 |

A language's `languages`, `min-word-length`, `messages` and `detect` replace the global ones, while
its `words` and `ignore-regexes` are added to the global lists.
