[
  (class_definition)
  (trait_definition)
  (object_definition)
  (enum_definition)
  (extension_definition)
  (given_definition)
  (function_definition)
  (val_definition)
  (var_definition)
  (while_expression)
  (do_while_expression)
  (for_expression)
  (match_expression)
  (case_clause)
  (catch_clause)
  (finally_clause)
  (block)
  (case_block)
  (arguments)
  (string)
  (interpolated_string)
  (block_comment)
  (import_declaration)
  (comment)+
] @fold

(import_declaration)+ @fold

; Scala 3's braceless `if`/`else` and `try` fold per clause, like their blocks do with braces.
(if_expression
  "if" @fold
  consequence: (_) @fold)

(if_expression
  "else" @fold
  alternative: (indented_block) @fold)

(try_expression
  "try" @fold
  body: (_) @fold)
