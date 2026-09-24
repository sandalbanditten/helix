; Adapted from context.hx: https://codeberg.org/gwid/context.hx
;
; Copyright (c) 2025 Gareth Widlansky
;
; Distributed under the MIT License.
; SPDX-License-Identifier: MIT

;; Lambda Context
(
  list
    .
    (symbol) @keyword
    (#any-of? @keyword "lambda" "λ")
    .
    (list)
    .
    _
) @breadcrumb

;; Constants
(
  list
    .
    (symbol) @keyword
    (#any-of? @keyword "define" "define-syntax" "define-macro")
    .
    (symbol) @name
) @breadcrumb

;; Functions
(
  list
    .
    (symbol) @keyword
    (#any-of? @keyword "define" "define/contract")
    .
    (list
      .
      (symbol) @function
      .
      (symbol)*
    )
) @breadcrumb
