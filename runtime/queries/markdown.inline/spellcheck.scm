; Markdown prose is injected into the `markdown.inline` layer as a single
; `(inline)` node, so checking that covers paragraphs, headings, list items,
; table cells and emphasised/linked text alike. The non-prose spans are then
; carved back out: inline code and math, link destinations and labels,
; autolinks, character references and HTML tags. Visible link text is left in.
(inline) @spell

[
  (code_span)
  (latex_block)
  (link_destination)
  (link_label)
  (uri_autolink)
  (email_autolink)
  (entity_reference)
  (numeric_character_reference)
  (html_tag)
] @nospell
