[
  (function_definition)
  (struct_specifier)
  (union_specifier)
  (enum_specifier)
  (case_statement)
  (compound_statement)
  (initializer_list)
  (argument_list)
  (gnu_asm_expression)
  (preproc_if)
  (preproc_ifdef "#ifdef")
  (preproc_elif)
  (preproc_elifdef)
  (preproc_else)
  (preproc_def)
  (preproc_function_def)
  (preproc_include)+
  (comment)+
] @fold

; `#ifndef` only below the top level, where it is usually an include guard spanning the file
(_ (_ (preproc_ifdef "#ifndef") @fold))
