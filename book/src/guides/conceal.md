## Adding conceal queries

Helix shows text as the symbol it stands for until a cursor comes close (see
[`[editor.conceal]`](../editor.md#editorconceal-section)):

```
$2 alpha^2$ and $x in RR$    →    $2 α^2$ and $x ∈ ℝ$
```

The text to conceal comes from a `conceals.scm` query file, and its symbols from a symbol table
built into Helix. Query files should be placed in `runtime/queries/{language}/conceals.scm` when
contributing to Helix. You may place these under your local runtime directory
(`~/.config/helix/runtime` in Linux for example) for the sake of testing.

If you're writing queries for the first time, be sure to check out the tree-sitter documentation
on [query syntax]. The `:tree-sitter-subtree` command shows the syntax tree under the primary
selection, which is the easiest way to find the names of the nodes to capture.

## Captures

### `@conceal`

Captures text to conceal. The pattern names the symbol table that the text is looked up in with
the `conceal.symbols` property:

```scm
((shorthand) @conceal
  (#set! conceal.symbols "typst-shorthand"))
```

Text that the table has no visible symbol for stays as it is. All nodes that one match captures
are concealed together, from the first to the last of them, so a pattern can hide a prefix along
with the symbol, like the `#` of Typst's `#sym.qed`:

```scm
((_ "#" @conceal . (field) @conceal @path)
  (#match? @path "^(sym|emoji)[.]")
  (#set! conceal.symbols "typst-code"))
```

Of conceals that nest, like `sym.arrow.r` and the `sym.arrow` inside it, only the outermost is
shown. Captures that span several lines or more than 64 bytes stay as they are.

### `@noconceal`

Captures a node whose text is not concealed, like a syntax error:

```scm
(ERROR) @noconceal
```

Only conceals that lie within the node are dropped. The Typst query captures the code after a `#`
in math this way, which leaves `#x` as it is, while `#sym.qed` still conceals as a whole because it
starts at the `#`, before the node.

## Symbol tables

| Table | Looks up | Examples |
| --- | --- | --- |
| `typst-math` | Typst's symbols as math refers to them, with modifiers in any order | `alpha` → `α`, `arrow.r.long` → `⟶` |
| `typst-code` | Typst's symbols and emoji as code refers to them, with or without a `#` | `sym.qed` → `∎`, `#emoji.face` → `😀` |
| `typst-shorthand` | Typst's shorthands of more than one character | `->` → `→`, `--` → `–` |

The Typst tables are those of Typst itself, so they cover every symbol it knows. Invisible
symbols, like spaces and zero-width characters, are never concealed.

The `conceals.scm` files in `runtime/queries` serve as examples.

[query syntax]: https://tree-sitter.github.io/tree-sitter/using-parsers/queries/1-syntax.html
