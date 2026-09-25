; Define collapse points

([
  (string)
  (external_command)
] @fold
  (#trim! @fold))

; Recipes fold from their name, which keeps attributes such as `[private]`
; out of the fold
((recipe
  name: (identifier) @fold
  (recipe_body) @fold)
  (#trim! @fold))

(comment)+ @fold
