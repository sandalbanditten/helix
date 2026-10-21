; Math in Markdown only: commands as the symbols they stand for, like `\alpha` as `α`
((generic_command) @conceal
  (#set! conceal.symbols "markdown-latex-command"))

; and scripts as superscripts and subscripts, like `x^2` as `x²`
((word) @conceal
  (#match? @conceal "[_^]")
  (#set! conceal.symbols "markdown-latex-scripts"))
