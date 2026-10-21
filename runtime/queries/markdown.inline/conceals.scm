; The delimiters of emphasis, strikethrough and inline code, like `**` and `` ` ``
([
  (emphasis_delimiter)
  (code_span_delimiter)
] @conceal
  (#set! conceal.symbols "markdown-hide"))

; Character references, like `&copy;` or `&#8594;`, as their characters
([
  (entity_reference)
  (numeric_character_reference)
] @conceal
  (#set! conceal.symbols "markdown-entity"))

; Links, images and autolinks, as an icon and their text
([
  (inline_link)
  (full_reference_link)
  (collapsed_reference_link)
  (image)
  (uri_autolink)
  (email_autolink)
] @conceal
  (#set! conceal.symbols "markdown-link"))

; Obsidian-style wikilinks, `[[note|alias]]`, as an icon and their alias or note
((_
  "[" @conceal .
  (shortcut_link) @conceal .
  "]" @conceal)
  (#set! conceal.symbols "markdown-wikilink"))

; Numeric footnote references, `[^1]`, in superscript
((shortcut_link) @conceal
  (#set! conceal.symbols "markdown-footnote"))
