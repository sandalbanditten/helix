((diff) @injection.content
 (#set! injection.language "diff"))

((rebase_command) @injection.content
 (#set! injection.include-children)
 (#set! injection.language "git-rebase"))

; The message's body, as Markdown
((message_line) @injection.content
 (#set! injection.combined)
 (#set! injection.language "markdown"))
