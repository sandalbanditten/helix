; Adapted from context.hx: https://codeberg.org/gwid/context.hx
;
; Copyright (c) 2025 Gareth Widlansky
;
; Distributed under the MIT License.
; SPDX-License-Identifier: MIT

;; Namespaces

(namespace_definition
  "namespace" @keyword
  name: (_)? @namespace) @breadcrumb


;; Type declarations
;; `body:` is required so forward declarations and plain uses of a type
;; don't enter the trail.

(class_specifier
  "class" @keyword
  name: (type_identifier) @type
  body: (field_declaration_list)) @breadcrumb

(struct_specifier
  "struct" @keyword
  name: (type_identifier) @type
  body: (field_declaration_list)) @breadcrumb

(union_specifier
  "union" @keyword
  name: (type_identifier) @type
  body: (field_declaration_list)) @breadcrumb

(enum_specifier
  "enum" @keyword
  "class"? @keyword
  name: (type_identifier) @type
  body: (enumerator_list)) @breadcrumb


;; Functions
;; The declarator chain wraps the name in `*f()`, `&f()`, and the name itself
;; is spelled differently for methods, `C::f`, `~C` and `operator=` -- `(_)`
;; takes whichever is there. The return type is optional: constructors and
;; destructors have none.

(function_definition
  type: (_)? @type
  declarator: [
    (function_declarator
      declarator: (_) @function)
    (pointer_declarator
      "*" @type
      declarator: (function_declarator declarator: (_) @function))
    (reference_declarator
      "&" @type
      (function_declarator declarator: (_) @function))
  ]) @breadcrumb


;; Preprocessor conditionals

;; `#ifndef` is deliberately not matched: it is how include guards are
;; spelled, and one would otherwise prefix every trail in a header.
(preproc_ifdef
  "#ifdef" @keyword.directive
  name: (identifier) @constant) @breadcrumb

(preproc_if
  "#if" @keyword.directive
  condition: (identifier) @constant) @breadcrumb
