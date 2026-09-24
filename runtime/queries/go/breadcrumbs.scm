; Adapted from context.hx: https://codeberg.org/gwid/context.hx
;
; Copyright (c) 2025 Gareth Widlansky
;
; Distributed under the MIT License.
; SPDX-License-Identifier: MIT

(function_declaration
  "func" @keyword.function
  name: (identifier) @function) @breadcrumb

(method_declaration
  "func" @keyword.function
  receiver: (parameter_list (parameter_declaration type: (_) @type))
  name: (field_identifier) @function) @breadcrumb


(type_declaration "type" @keyword
  (type_spec
    name: (_) @type
    type: [(struct_type "struct" @keyword) (interface_type "interface" @keyword)]
  )
) @breadcrumb
