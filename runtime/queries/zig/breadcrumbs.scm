; Adapted from context.hx: https://codeberg.org/gwid/context.hx
;
; Copyright (c) 2025 Gareth Widlansky
;
; Distributed under the MIT License.
; SPDX-License-Identifier: MIT

(function_declaration
  ("pub" @keyword)?
  "fn" @keyword.function
  name: (identifier) @function
) @breadcrumb

(variable_declaration
  (identifier) @type
  .
  "="
  .
  [(struct_declaration "struct" @keyword) (enum_declaration "enum" @keyword)]
) @breadcrumb
