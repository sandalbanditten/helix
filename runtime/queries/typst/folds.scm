[
  (section)
  (call)
  (group)
  (block)
  (content)
  (raw_blck)
  (math)
  (item)
  (term)
  (comment)+
] @fold

; Each `#import` sits between a `#` and a statement end, which a plain `(import)+` doesn't skip.
("#"
  .
  (import) @fold
  .
  "end")+
