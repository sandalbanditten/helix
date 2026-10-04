## Adding conceal queries

Helix shows text as the symbol it stands for until the cursor comes close (see
[`[editor.conceal]`](../editor.md#editorconceal-section)):

```
$2 alpha^2$ and $x in RR$    →    $2 α^2$ and $x ∈ ℝ$
```

The text to conceal comes from a `conceals.scm` query file, and its symbols from a symbol table
built into Helix. Query files should be placed in `runtime/queries/{language}/conceals.scm` when
contributing to Helix. You may place these under your local runtime directory
(`~/.config/helix/runtime` in Linux for example) for the sake of testing.

If you're writing queries for the first time, be sure to check out the tree-sitter documentation
on [query syntax].

## Captures

### `@conceal`

Captures text to conceal. The `conceal.symbols` property names the symbol table the text is looked
up in:

```scm
((shorthand) @conceal
  (#set! conceal.symbols "typst-shorthand"))
```

The nodes one match captures are concealed together, so a pattern can hide a prefix along with
the symbol, like the `#` of Typst's `#sym.qed`:

```scm
((_ "#" @conceal . (field) @conceal @path)
  (#match? @path "^(sym|emoji)[.]")
  (#set! conceal.symbols "typst-code"))
```

### `@noconceal`

Captures a node whose text is not concealed, like a syntax error:

```scm
(ERROR) @noconceal
```

## Symbol tables

| Table | Looks up | Examples |
| --- | --- | --- |
| `typst-math` | Typst's symbols as math refers to them | `alpha` → `α`, `arrow.r.long` → `⟶` |
| `typst-code` | Typst's symbols and emoji as code refers to them | `sym.qed` → `∎`, `#emoji.face` → `😀` |
| `typst-shorthand` | Typst's shorthands | `->` → `→`, `--` → `–` |

The `conceals.scm` files in `runtime/queries` serve as examples.

[query syntax]: https://tree-sitter.github.io/tree-sitter/using-parsers/queries/1-syntax.html
