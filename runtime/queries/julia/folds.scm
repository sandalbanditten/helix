[
  (module_definition)
  (struct_definition)
  (macro_definition)
  (function_definition)
  (if_statement)
  (try_statement)
  (for_statement)
  (while_statement)
  (let_statement)
  (quote_statement)
  (do_clause)
  (compound_statement) ; begin block
  (argument_list)
  (vector_expression)
  (matrix_expression)
  (tuple_expression)
  (comprehension_expression)
  (string_literal) ; docstrings
  (block_comment)
  (line_comment)+
] @fold

[
  (using_statement)
  (import_statement)
]+ @fold
