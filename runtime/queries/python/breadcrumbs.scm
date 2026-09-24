; Adapted from context.hx: https://codeberg.org/gwid/context.hx
;
; Copyright (c) 2025 Gareth Widlansky
;
; Distributed under the MIT License.
; SPDX-License-Identifier: MIT

(function_definition
  "def" @keyword.function
  name: (identifier) @function) @breadcrumb


(class_definition
  "class" @keyword
  name: (identifier) @type) @breadcrumb
