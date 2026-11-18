; Only the injections rsx bodies use, not all of Rust's, so the query compiles fast.

([(line_comment !doc) (block_comment !doc)] @injection.content
 (#set! injection.language "comment"))

((doc_comment) @injection.content
 (#set! injection.language "markdown-rustdoc")
 (#set! injection.combined))

((macro_invocation
  (token_tree) @injection.content)
 (#set! injection.language "rust")
 (#set! injection.include-children))

((macro_invocation
   macro:
     [
       (scoped_identifier
         name: (_) @_macro_name)
       (identifier) @_macro_name
     ]
   (token_tree) @injection.content)
 (#eq? @_macro_name "rsx")
 (#set! injection.language "rsx")
 (#set! injection.include-children))

; Special language `tree-sitter-rust-format-args` for Rust macros,
; which use `format_args!` under the hood and therefore have
; the `format_args!` syntax.
;
; This language is injected into a hard-coded set of macros.
(
  (macro_invocation
    macro:
      [
        (scoped_identifier
          name: (_) @_macro_name)
        (identifier) @_macro_name
      ]
    (token_tree) @injection.content
  )
  (#any-of? @_macro_name
    ; 1st argument is `format_args!`

    ; std
    "print" "println" "eprint" "eprintln"
    "format" "format_args" "todo" "panic"
    "unreachable" "unimplemented" "compile_error"
    ; log
    "crit" "trace" "debug" "info" "warn" "error"
    ; anyhow
    "anyhow" "bail"
    ; syn
    "format_ident"
    ; indoc
    "formatdoc" "printdoc" "eprintdoc" "writedoc"
    ; iced
    "text"
    ; ratatui
    "span"
    ; eyre
    "eyre"
    ; miette
    "miette"

    ; 2nd argument is `format_args!`

    ; std
    "write" "writeln" "assert" "debug_assert"
    ; defmt
    "expect" "unwrap"
    ; ratatui
    "span"

    ; 3rd argument is `format_args!`

    ; std
    "assert_eq" "debug_assert_eq" "assert_ne" "debug_assert_ne"
  )
  (#set! injection.language "rust-format-args-macro")
  (#set! injection.include-children)
)

; Dioxus formats every string like `format_args!`.
([
   (string_literal (string_content) @injection.content)
   (raw_string_literal (string_content) @injection.content)
 ]
 (#set! injection.language "rust-format-args")
 (#set! injection.include-children))
