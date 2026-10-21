## Adding conceal queries

Helix shows text as the symbol it stands for until the cursor comes close (see
[`[editor.conceal]`](../editor.md#editorconceal-section)):

```
$2 alpha^2$ and $x in RR$    →    $2 α^2$ and $x ∈ ℝ$
```

The text to conceal comes from a `conceals.scm` query file, and what it is shown as from a symbol
table built into Helix. Query files should be placed in `runtime/queries/{language}/conceals.scm` when
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
| `typst-wikilink` | Obsidian-style wikilinks, without their brackets and escapes | `[[Note\#Heading]]` → `Note#Heading` |
| `typst-rule` | Lines across the page, as a rule filling the row | `#line(length: 100%)` → `────` |
| `markdown-hide` | Nothing: the captured text is hidden | `**bold**` → `bold` |
| `markdown-entity` | Character references, as their characters | `&rarr;` → `→` |
| `markdown-link` | Links, images and autolinks, as an icon and their text | `[docs](https://x.org)` → `󰖟 docs` |
| `markdown-wikilink` | Obsidian-style wikilinks, as an icon and their alias or note | `[[Note\|alias]]` → `󱗖 alias` |
| `markdown-footnote` | Numeric footnote references, in superscript | `[^12]` → `¹²` |
| `markdown-rule` | Thematic breaks, as a rule filling the row | `---` → `────` |
| `markdown-heading` | ATX headings, with their `#`s as an icon of their level | `## Title` → ` 󰲣 Title` |
| `markdown-list-marker` | List markers: bullets, numbers counted along the list and task checkboxes | `- [x] done` → `󰱒  done` |
| `markdown-quote-marker` | The `>`s of quote markers and continuations, as bars | `> quote` → `▎ quote` |
| `markdown-callout` | Block quotes starting with `[!KIND]`, with their kind as an icon and title | `> [!NOTE]` → `▎ 󰋽 Note` |
| `markdown-latex-command` | LaTeX math commands in Markdown, as their symbols, with the script after them | `\alpha` → `α`, `\mathbb{R}` → `ℝ`, `\sum_{i=1}` → `∑ᵢ₌₁` |
| `markdown-latex-scripts` | The superscripts and subscripts of a word of LaTeX math in Markdown | `x^2` → `x²`, `a_1` → `a₁` |
| `markdown-code-fence` | Fenced code blocks, with their opening fence as their language's icon, info and a rule, and their closing fence hidden | ` ```rust ` → ` rust ────` |

The `markdown-` tables draw Markdown rendered, as [`[editor.markdown]`](../editor.md#editormarkdown-section)
says, and their conceals are revealed while the cursor is on their line.

The `conceals.scm` files in `runtime/queries` serve as examples.

[query syntax]: https://tree-sitter.github.io/tree-sitter/using-parsers/queries/1-syntax.html
