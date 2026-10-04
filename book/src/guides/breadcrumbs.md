## Adding breadcrumb queries

Helix shows the syntax nodes enclosing the cursor, such as the module, type and function it is
in, as breadcrumbs in the statusline:

```
> mod editor > impl Editor > pub fn render
```

The breadcrumbs of a language come from a `breadcrumbs.scm` query file. Query files should be
placed in `runtime/queries/{language}/breadcrumbs.scm` when contributing to Helix. You may place
these under your local runtime directory (`~/.config/helix/runtime` in Linux for example) for the
sake of testing.

## Captures

### `@breadcrumb`

Captures a node that becomes a breadcrumb when it encloses the cursor.

### Other captures

The other captures of the pattern make up the breadcrumb's text, in document order. Each is
styled with the [theme scope](../themes.md#scopes) of its name. This decides which parts of a node
are shown, for example the keyword and name of a function but not its parameters:

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

The `breadcrumbs.scm` files in `runtime/queries` serve as examples.
