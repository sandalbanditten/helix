[
  (class_definition)
  (function_definition)
  (constructor_definition)
  (lambda)
  (enum_definition)
  (for_statement)
  (while_statement)
  (match_statement)
  (pattern_section)
  (elif_clause)
  (else_clause)
  (setget)
  (get_body)
  (set_body)
  (arguments)
  (array)
  (dictionary)
  (comment)+
] @fold

; An `if` with `elif` or `else` clauses folds per clause.
(if_statement
  "if" @fold
  body: (_) @fold)

; Multi-line strings used as comments
(expression_statement
  (string) @fold)
