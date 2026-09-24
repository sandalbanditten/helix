; Adapted from context.hx: https://codeberg.org/gwid/context.hx
;
; Copyright (c) 2025 Gareth Widlansky
;
; Distributed under the MIT License.
; SPDX-License-Identifier: MIT

(function_definition
 type:
 (_)
 @type.builtin
 declarator:
 [(function_declarator declarator: [(identifier) (field_identifier)] @function)
  (pointer_declarator "*" @type.builtin
                      declarator:
                      (function_declarator declarator: [(identifier) (field_identifier)] @function))])
@breadcrumb

(preproc_ifdef "#ifdef" @keyword.directive name: (identifier) @constant)
@breadcrumb

(preproc_if "#if" @keyword.directive condition: (identifier) @constant)
@breadcrumb
