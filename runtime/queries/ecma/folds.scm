[
  (function_declaration)
  (function_expression)
  (generator_function)
  (generator_function_declaration)
  (arrow_function)
  (class_body)
  (statement_block)
  (switch_body)
  (switch_case)
  (switch_default)
  (variable_declarator)
  (arguments)
  (parenthesized_expression)
  (object)
  (object_pattern)
  (array)
  (array_pattern)
  (named_imports)
  (export_clause)
  (template_string)
  (import_statement)+
  (comment)+
] @fold

(class_declaration
  name: (_) @fold
  body: (_) @fold)

(method_definition
  name: (_) @fold
  body: (_) @fold)
