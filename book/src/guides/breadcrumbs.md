## Adding breadcrumb queries

Helix shows the syntax nodes enclosing the cursor, such as the module, type
and function it is in, as a trail of breadcrumbs in the `breadcrumbs`
statusline element:

```
> mod editor > impl Editor > pub fn render
```

The breadcrumbs of a language come from a `breadcrumbs.scm` query file. Query
files should be placed in `runtime/queries/{language}/breadcrumbs.scm` when
contributing to Helix. You may place these under your local runtime directory
(`~/.config/helix/runtime` in Linux for example) for the sake of testing.

## Captures

### `@breadcrumb`

Captures a node that becomes a breadcrumb whenever it encloses the cursor.

### Any other capture

Every other capture in the pattern of a `@breadcrumb` becomes part of its
text, in document order and separated by spaces. The capture's name is the
[theme scope](../themes.md#scopes) the text is styled with: `@keyword.function`
falls back to `keyword` if the theme does not define it, and to the statusline
style if it defines neither. The query therefore
decides which parts of a node are shown, for example the keyword and name of a
function but not its parameters:

```scm
(function_item
  ((visibility_modifier _) @keyword)?
  "fn" @keyword.function
  name: _ @function) @breadcrumb

(section
  .
  (atx_heading
    heading_content: _ @markup.heading)) @breadcrumb
```

Captured text that spans several lines is joined into one: runs of whitespace
become a single space, and line breaks and padding just inside brackets are
dropped along with a trailing comma before a closing bracket, so that
`Cache<\n    K,\n    V,\n>` is shown as `Cache<K, V>`. Text longer than 512 bytes is cut
off with `…`.

Breadcrumbs of injected languages follow those of the language they are
injected into, such as the function around the cursor in a code block of a
Markdown section.

The `breadcrumbs.scm` files in `runtime/queries` serve as examples.
