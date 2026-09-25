[
  (function_definition)
  (class_definition)
  (with_statement)
  (match_statement)
  (case_clause)
  (elif_clause)
  (else_clause)
  (except_clause)
  (finally_clause)
  (import_from_statement)
  (argument_list)
  (parenthesized_expression)
  (generator_expression)
  (list_comprehension)
  (set_comprehension)
  (dictionary_comprehension)
  (tuple)
  (list)
  (set)
  (dictionary)
  (string)
  (comment)+
] @fold

; Statements with `elif`, `else`, `except` or `finally` clauses fold per clause.
(if_statement
  "if" @fold
  consequence: (_) @fold)

(for_statement
  "for" @fold
  body: (_) @fold)

(while_statement
  "while" @fold
  body: (_) @fold)

(try_statement
  "try" @fold
  body: (_) @fold)

[
  (import_statement)
  (import_from_statement)
]+ @fold
