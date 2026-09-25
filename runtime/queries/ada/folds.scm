; Support for folding in Ada
;;    za     toggles folding a package, subprogram, if statement or loop

[
   (package_declaration)
   (generic_package_declaration)
   (package_body)
   (subprogram_declaration)
   (subprogram_body)
   (task_type_declaration)
   (single_task_declaration)
   (task_body)
   (protected_type_declaration)
   (single_protected_declaration)
   (protected_body)
   (entry_body)
   (full_type_declaration)
   (block_statement)
   (if_statement)
   (case_statement)
   (loop_statement)
   (extended_return_statement)
   (gnatprep_declarative_if_statement)
   (gnatprep_if_statement)
   (comment)+
] @fold

; Context clauses, each of which is a compilation unit of its own
((compilation_unit
   [
      (with_clause)
      (use_clause)
   ])+ @fold)
