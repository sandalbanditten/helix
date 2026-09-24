; Adapted from context.hx: https://codeberg.org/gwid/context.hx
;
; Copyright (c) 2025 Gareth Widlansky
;
; Distributed under the MIT License.
; SPDX-License-Identifier: MIT

;; Type declarations
;; `type_synomym` is spelled that way in the grammar, not a typo here.

(data_type
  "data" @keyword
  name: (name) @type) @breadcrumb

(newtype
  "newtype" @keyword
  name: (name) @type) @breadcrumb

(type_synomym
  "type" @keyword
  name: (name) @type) @breadcrumb


;; Classes and instances

(class
  "class" @keyword
  name: (name) @type) @breadcrumb

;; The instance head carries the type it is for -- `instance Describable Shape`
;; -- which is what tells several instances of one class apart.
(instance
  "instance" @keyword
  name: (name) @type
  patterns: (type_patterns)? @type) @breadcrumb


;; Bindings
;; Three spellings of one idea: `f x = ...` is a `function`, `f = ...` with no
;; arguments a `bind` (which is what `main = do ...` is), and `f :: T` a
;; `signature`. Do-statements are `bind`s too but hold a `pattern:` instead of
;; a `name:`, so `x <- action` stays out of the trail.
;;
;; There is one node per equation, so the clauses of `f (A x) = ...` /
;; `f (B y) = ...` stand on their own, and `where`-bound helpers nest inside
;; the binding they belong to.

[
  (function name: (variable) @function)
  (bind name: (variable) @function)
  (signature name: (variable) @function)
] @breadcrumb
