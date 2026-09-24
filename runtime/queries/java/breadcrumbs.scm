; Adapted from context.hx: https://codeberg.org/gwid/context.hx
;
; Copyright (c) 2025 Gareth Widlansky
;
; Distributed under the MIT License.
; SPDX-License-Identifier: MIT

;; Type declarations

(class_declaration
  "class" @keyword
  name: (identifier) @type) @breadcrumb

(interface_declaration
  "interface" @keyword
  name: (identifier) @type) @breadcrumb

(annotation_type_declaration
  "@interface" @keyword
  name: (identifier) @type) @breadcrumb

(enum_declaration
  "enum" @keyword
  name: (identifier) @type) @breadcrumb

(record_declaration
  "record" @keyword
  name: (identifier) @type) @breadcrumb


;; Members

;; Java has no `fn`/`def` keyword, so the return type fills the same slot,
;; the way C leads a function with its type.
(method_declaration
  type: (_) @type
  name: (identifier) @function) @breadcrumb

(constructor_declaration
  name: (identifier) @function) @breadcrumb

;; `record Point { ... }` -- a constructor without a parameter list
(compact_constructor_declaration
  name: (identifier) @function) @breadcrumb

;; `@interface Marker { String value(); }`
(annotation_type_element_declaration
  type: (_) @type
  name: (identifier) @function) @breadcrumb
