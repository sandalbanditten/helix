; inherits: rust

(element
  name: [(identifier) (custom_element_name)] @tag
  (#not-match? @tag "^[A-Z]|_")) @breadcrumb

(element
  name: _ @constructor
  (#match? @constructor "^[A-Z]|_|::")) @breadcrumb
