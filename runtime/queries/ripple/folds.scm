; Folds for code blocks
[
  (statement_block)
  (component_body)
  (class_body)
  (object)
  (object_pattern)
  (object_type)
  (array)
  (array_pattern)
  (reactive_array)
  (reactive_object)
  (switch_body)
  (arguments)
  (named_imports)
  (export_clause)
] @fold

; Fold runs of imports
(import_statement)+ @fold

; Fold multi-line JSX elements
(jsx_element) @fold

; Fold style elements
(style_element) @fold

; Fold server blocks
(server_block) @fold

; Fold comments
(comment)+ @fold

; Fold template strings
(template_string) @fold
