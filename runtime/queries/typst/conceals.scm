; Nothing in a syntax error is concealed, like the text after an unclosed `$`
(ERROR) @noconceal

; Code after `#` in math, like `#x`, names no math symbol
(_ "#" . [(ident) (field)] @noconceal)

; Symbols in math, like `alpha` or `arrow.r.long`. A symbol that is called, like the accent in
; `dot(x)`, is left as it is.
([
  (formula [(ident) (field)] @conceal)
  (attach [(ident) (field)] @conceal)
  (fraction [(ident) (field)] @conceal)
  (root [(ident) (field)] @conceal)
  (prime [(ident) (field)] @conceal)
  (fac [(ident) (field)] @conceal)
] (#set! conceal.symbols "typst-math"))

; Symbols and emoji in code, like `sym.qed` or `emoji.face`, with a `#` before them
((_ "#" @conceal . (field) @conceal @path)
  (#match? @path "^(sym|emoji)[.]")
  (#set! conceal.symbols "typst-code"))

((field) @conceal
  (#match? @conceal "^(sym|emoji)[.]")
  (#set! conceal.symbols "typst-code"))

; Shorthands, like `->` or `!=` in math and `--` in markup
((shorthand) @conceal
  (#set! conceal.symbols "typst-shorthand"))

((symbol) @conceal
  (#eq? @conceal "||")
  (#set! conceal.symbols "typst-shorthand"))

; Shorthands the grammar splits, like `==>` into `=` and `=>`, as a whole
((_
  [(symbol) (shorthand)] @conceal .
  [(symbol) (shorthand)] @conceal)
  (#set! conceal.symbols "typst-shorthand"))

((_
  [(symbol) (shorthand)] @conceal .
  [(symbol) (shorthand)] @conceal .
  [(symbol) (shorthand)] @conceal)
  (#set! conceal.symbols "typst-shorthand"))

((group ["(" ")"] @conceal)
  (#any-of? @conceal "[|" "|]")
  (#set! conceal.symbols "typst-shorthand"))

; Obsidian-style wikilinks, like `[[Note\#Heading|alias]]`, without their brackets and the
; backslashes of their escapes
(_
  (text) @conceal @open .
  (text) @conceal @open.second .
  (text) @conceal .
  (text) @conceal @close .
  (text) @conceal @close.second
  (#eq? @open "[")
  (#eq? @open.second "[")
  (#eq? @close "]")
  (#eq? @close.second "]")
  (#set! conceal.symbols "typst-wikilink"))

; Lines across the page, `#line(length: 100%)`, as a rule filling the row
((_ "#" @conceal . (call . (ident) @function) @conceal)
  (#eq? @function "line")
  (#set! conceal.symbols "typst-rule"))
