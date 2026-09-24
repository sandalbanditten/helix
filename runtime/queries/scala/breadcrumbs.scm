; Adapted from context.hx: https://codeberg.org/gwid/context.hx
;
; Copyright (c) 2025 Gareth Widlansky
;
; Distributed under the MIT License.
; SPDX-License-Identifier: MIT

;; Templates
;; `case` is optional so `case class Point` and `case object Empty` read as
;; they are written.

(trait_definition
  "trait" @keyword
  name: (identifier) @type) @breadcrumb

(class_definition
  "case"? @keyword
  "class" @keyword
  name: (identifier) @type) @breadcrumb

(object_definition
  "case"? @keyword
  "object" @keyword
  name: (identifier) @type) @breadcrumb

(enum_definition
  "enum" @keyword
  name: (identifier) @type) @breadcrumb


;; Methods
;; An abstract `def` in a trait is a `function_declaration`, one with a body a
;; `function_definition`. Nested defs are ordinary children, so a local helper
;; extends the trail on its own.

[
  (function_definition
    "def" @keyword.function
    name: (identifier) @function)
  (function_declaration
    "def" @keyword.function
    name: (identifier) @function)
] @breadcrumb
