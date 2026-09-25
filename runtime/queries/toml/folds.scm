; A table ends at its last pair, not at the comments before the next table.
(table
  [(bare_key) (dotted_key) (quoted_key)] @fold
  [(pair) @fold (comment)]+)

(table_array_element
  [(bare_key) (dotted_key) (quoted_key)] @fold
  [(pair) @fold (comment)]+)

[
  (array)
  (comment)+
] @fold
