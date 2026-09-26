; Prose is made of `text` nodes. Commands, math and code sit inside or next to
; them, so they are carved back out, as are the arguments that name things
; rather than hold prose: environment names, options, labels, references,
; citations and definitions.
(text) @spell

[
  (command_name)
  (inline_formula)
  (displayed_equation)
  (math_environment)
  (verbatim_environment)
  (listing_environment)
  (minted_environment)
  (pycode_environment)
  (begin)
  (end)
  (key_value_pair)
  (label_definition)
  (label_reference)
  (label_reference_range)
  (citation)
  (acronym_reference)
  (glossary_entry_reference)
  (color_reference)
  (color_definition)
  (color_set_definition)
] @nospell

(new_command_definition declaration: _ @nospell)
(environment_definition name: _ @nospell)
(theorem_definition name: _ @nospell)
(glossary_entry_definition name: _ @nospell)
(acronym_definition name: _ @nospell)
