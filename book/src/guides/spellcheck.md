# Adding spellcheck queries

Helix uses `spellcheck.scm` query files to decide which parts of a document are spell checked (see
[Spell checking](../spell-checking.md)). The captures follow the convention of
[nvim-treesitter](https://github.com/nvim-treesitter/nvim-treesitter). Query files should be
placed in `runtime/queries/{language}/spellcheck.scm` when contributing to Helix. You may place
these under your local runtime directory (`~/.config/helix/runtime` in Linux for example) for the
sake of testing.

If you're writing queries for the first time, be sure to check out the tree-sitter documentation
on [query syntax]. The `:tree-sitter-subtree` command shows the syntax tree under the primary
selection, which is the easiest way to find the names of the nodes to capture.

## Captures

### `@spell`

Captures a node whose text is checked:

```scm
(paragraph) @spell
```

### `@nospell`

Captures a node whose text is not checked, even within an `@spell` node:

```scm
[
  (code_span)
  (link_destination)
] @nospell
```

`@nospell` applies across injected languages too: the Markdown query skips code blocks, which also
skips the comments of the languages injected into them.

## Comments

Most languages inject the `comment` grammar into their comments, and its query checks the comment
text. A language therefore needs no query of its own to have its comments checked. A language with
a syntax tree has nothing else checked unless its query captures it, while a document without a
syntax tree, like plain text, is checked in full.

[query syntax]: https://tree-sitter.github.io/tree-sitter/using-parsers/queries/1-syntax.html
