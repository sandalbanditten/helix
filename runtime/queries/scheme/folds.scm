[
  (block_comment)
  (comment)+
] @fold

(program
  [
    (list)
    (quote)
    (quasiquote)
    (vector)
  ] @fold)

; A nested form folds unless it is its list's first element or first argument, which usually
; start on the line of the enclosing form: `(let ((a 1)` or `(cond ((null? x)`.
(list
  .
  (_)
  .
  (_)
  [
    (list)
    (quote)
    (quasiquote)
    (vector)
  ] @fold)

(list
  .
  (list)
  .
  [
    (list)
    (quote)
    (quasiquote)
    (vector)
  ] @fold)
