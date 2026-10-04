# Adding spellcheck queries

Helix uses `spellcheck.scm` query files to decide which parts of a document are spell checked (see
[Spell checking](../spell-checking.md)). Query files should be placed in
`runtime/queries/{language}/spellcheck.scm` when contributing to Helix. You may place these under
your local runtime directory (`~/.config/helix/runtime` in Linux for example) for the sake of
testing.

If you're writing queries for the first time, be sure to check out the tree-sitter documentation
on [query syntax].

## Captures

### `@spell`

Captures a node whose text is checked:

```scm
(paragraph) @spell
```

### `@nospell`

Captures a node whose text is not checked, even within an `@spell` node, including the languages
injected into it:

```scm
[
  (code_span)
  (link_destination)
] @nospell
```

Comments are checked by the query of the `comment` grammar, which most languages inject into their
comments, so a language needs no query of its own for them.

[query syntax]: https://tree-sitter.github.io/tree-sitter/using-parsers/queries/1-syntax.html
