[
  (braced_word)
  (command_substitution)
] @fold

; Runs of comments. Comments are commands here, separated by "\n" tokens that
; a plain `(comment)+` does not skip.
((comment) @fold
  .
  ("\n" . (comment) @fold)*)
