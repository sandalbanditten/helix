; inherits: rust

; Dioxus formats every string like `format_args!`.
([
   (string_literal (string_content) @injection.content)
   (raw_string_literal (string_content) @injection.content)
 ]
 (#set! injection.language "rust-format-args")
 (#set! injection.include-children))
