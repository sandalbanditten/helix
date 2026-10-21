; Thematic breaks, `---`, as a rule across the row
((thematic_break) @conceal
  (#set! conceal.symbols "markdown-rule"))

; ATX headings, `## Title`, with their `#`s as an icon of their level
((atx_heading) @conceal
  (#set! conceal.symbols "markdown-heading"))

; List markers: bullets, numbers counted along the list and task checkboxes
([
  (list_marker_minus)
  (list_marker_plus)
  (list_marker_star)
  (list_marker_dot)
  (list_marker_parenthesis)
] @conceal
  (#set! conceal.symbols "markdown-list-marker"))

; The `>`s of block quotes, as bars
([
  (block_quote_marker)
  (block_continuation)
] @conceal
  (#set! conceal.symbols "markdown-quote-marker"))

; Callouts, `> [!NOTE]`, with their kind as an icon and title
((block_quote) @conceal
  (#set! conceal.symbols "markdown-callout"))
