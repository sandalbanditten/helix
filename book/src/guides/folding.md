## Adding fold queries

Helix folds syntax nodes such as functions, types and blocks: a closed fold hides the node's lines
after its first one, which ends in a placeholder instead (see
[`[editor.folding]`](../editor.md#editorfolding-section)):

```
fn new() -> Self {…}
if self.done() {…} else {…}
```

The nodes that can be folded come from a `folds.scm` query file, which follows the convention of
[nvim-treesitter](https://github.com/nvim-treesitter/nvim-treesitter). Query files should be
placed in `runtime/queries/{language}/folds.scm` when contributing to Helix. You may place these
under your local runtime directory (`~/.config/helix/runtime` in Linux for example) for the sake of
testing.

## Captures

### `@fold`

Captures a node that can be folded:

```scm
[
  (function_item)
  (impl_item)
  (block)
] @fold
```

All nodes that one match captures form a single region, from the first to the last of them. A
quantified capture therefore folds a run of siblings, such as the imports at the top of a file,
and a pattern with several captures folds the part of a node between them, such as an `if` clause
from its keyword to the end of its body:

```scm
(use_declaration)+ @fold

(if_statement
  "if" @fold
  consequence: (_) @fold)
```

## How a region is folded

A fold always starts at the end of the region's first line, which stays visible, and ends in one
of two ways:

- If the region's last line starts with a closing bracket (`)`, `]` or `}`) that the syntax tree
  pairs with a bracket on the first line, the fold ends right before it, and the bracket is pulled
  onto the first line: `class Lexer {…};`.
- Otherwise the fold hides everything up to the region's last non-whitespace character:
  `def f():…` or `main = do…`.

Text after the region on its last line stays visible, and so do blank lines at its end and
comments that end it without being indented deeper than its first line (tree-sitter puts the
comments before the next, less indented line into the preceding block). A region that doesn't
span at least two lines with content is not foldable, and regions that fold identically are
merged.

Toggling a fold at the cursor closes the smallest region that starts on the cursor's line, or
else the smallest one containing it. Some captures are therefore better left out:

- Captures that only repeat another capture's fold, like a loop whose fold is the same as that
  of its body.
- Captures that nest many regions on the same line, like the calls of a method chain split over
  several lines; capture their argument lists instead.
- Whole `if` statements and `try` statements: folding the blocks of their clauses keeps
  `if a {…} else {…}` on one line, where a fold of the statement would hide all of it.

## Predicates

`#trim!` is accepted for compatibility with nvim-treesitter and does nothing, as folds never hide
blank lines at the end of a region. Other custom predicates are not supported.

The `folds.scm` files in `runtime/queries` serve as examples.
