# Spell checking

Helix can spell check the prose of documents, such as the text of Markdown, LaTeX and Typst
documents and the comments of code. Misspellings are underlined, and their corrections are offered
as code actions.

## Enabling

Spell checking is enabled by choosing the languages to check against, in one of these ways, from
the highest precedence to the lowest:

1. Per buffer, with `:set-spelling-language` (`:spelling`), like `:spelling en_US`. `off` disables
   checking, and no argument shows the languages in use.

2. Per path, with the `spelling_language` property of `.editorconfig`:

   ```ini
   [*.md]
   spelling_language = en-US
   ```

3. Per language, in `languages.toml`:

   ```toml
   [[language]]
   name = "markdown"
   spelling.languages = ["en_US"]
   ```

4. Globally, in [`[editor.spelling]`](./editor.md#editorspelling-section):

   ```toml
   [editor.spelling]
   languages = ["en_US"]
   ```

With several languages, a word is accepted when any of them knows it. With `detect = true`, each
document is checked against the language it is written in.

## Usage

| Key        | Description                           |
| ---        | ---                                   |
| `]s`, `[s` | Go to the next, previous misspelling  |
| `]S`, `[S` | Go to the last, first misspelling     |
| `Space A`  | Fix the misspelling under the cursor  |

The fixes, the suggested corrections and adding the word to your dictionary, are also among the
code actions of `Space a`.

Which parts of a document are checked is decided per language by a `spellcheck.scm` query (see
[Adding spellcheck queries](./guides/spellcheck.md)). Documents without one, like plain text, are
checked in full.

## Dictionaries

Helix comes with dictionaries for `en_US` and `da_DK`. Others can be added as Hunspell `.aff` and
`.dic` files in a [runtime directory](./building-from-source.md#configuring-helixs-runtime-files),
named after the language:

```
~/.config/helix/runtime/dictionaries/de_DE/de_DE.aff
~/.config/helix/runtime/dictionaries/de_DE/de_DE.dic
```

Abbreviations, like `dvs.` and `f.eks.` in `da_DK`, are checked with their dots.

Words added to your dictionary are kept in `~/.local/state/helix/dictionaries/<language>.txt` on
Linux, one per line.
