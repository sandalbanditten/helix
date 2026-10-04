## Adding fold queries

Helix folds syntax nodes such as functions, types and blocks, hiding them behind their first line
(see [`[editor.folding]`](../editor.md#editorfolding-section)):

```
fn new() -> Self {…}
if self.done() {…} else {…}
```

The nodes that can be folded come from a `folds.scm` query file. Query files should be placed in
`runtime/queries/{language}/folds.scm` when contributing to Helix. You may place these under your
local runtime directory (`~/.config/helix/runtime` in Linux for example) for the sake of testing.

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

The nodes one match captures form a single region, from the first to the last of them. A
quantified capture folds a run of siblings, such as the imports at the top of a file, and a
pattern with several captures folds the part of a node between them:

```scm
(use_declaration)+ @fold

(if_statement
  "if" @fold
  consequence: (_) @fold)
```

A fold keeps the first line of its region visible. When the region ends with a closing bracket
or a comment delimiter, that is shown after the placeholder: `class Lexer {…};`, `/**…*/`.

Toggling a fold folds the smallest region that starts on the cursor's line, so leave out captures
that fold the same lines as another one, or that start many regions on the same line, like the
calls of a method chain.

The `folds.scm` files in `runtime/queries` serve as examples.
