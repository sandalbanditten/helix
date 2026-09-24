; Adapted from context.hx: https://codeberg.org/gwid/context.hx
;
; Copyright (c) 2025 Gareth Widlansky
;
; Distributed under the MIT License.
; SPDX-License-Identifier: MIT

(mod_item
  ((visibility_modifier _) @keyword)?
  "mod" @keyword
  name: (identifier) @namespace
) @breadcrumb

(impl_item
  "impl" @keyword
  type: (_) @type
  !trait
) @breadcrumb

;; with trait
(impl_item
  "impl" @keyword
  trait: (_) @type
  "for" @keyword
  type: (_) @type
) @breadcrumb

(struct_item
  ((visibility_modifier _) @keyword)?
  "struct" @keyword
  name: (_) @type
) @breadcrumb


(enum_item
  ((visibility_modifier _) @keyword)?
  "enum" @keyword
  name: (_) @type
) @breadcrumb

(function_item
  ((visibility_modifier _) @keyword)?
  "fn" @keyword.function
  name: _ @function
) @breadcrumb
