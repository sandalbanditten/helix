; Text nodes cover prose, headings, emphasis, and content blocks, including
; content passed to functions. Code identifiers, strings, labels, and URLs do not.
; A quote is its own node, so it is checked too to keep contractions like
; "don't" in one piece.
[
  (text)
  (quote)
] @spell

[
  (escape)
  (raw_span)
  (raw_blck)
  (math)
] @nospell
