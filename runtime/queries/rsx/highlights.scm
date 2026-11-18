; inherits: rust

(element name: (identifier) @tag)
(element name: (custom_element_name [(identifier) "-"] @tag))

; Dioxus renders lowercase names without underscores as elements, the rest as components.
(element
  name: ((identifier) @constructor
    (#match? @constructor "^[A-Z]|_")))
(element name: (scoped_identifier name: (identifier) @constructor))
(element
  name: (generic_type_with_turbofish
    type: [
      (identifier) @constructor
      (scoped_identifier name: (identifier) @constructor)
    ]))

(rsx_attribute name: (identifier) @attribute)
(rsx_attribute name: (string_literal) @attribute)
