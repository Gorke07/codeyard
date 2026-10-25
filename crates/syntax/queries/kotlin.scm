; Highlight query for Kotlin, original to this project: written from the node types of tree-sitter-kotlin-ng,
; which ships no highlights query. Where two patterns capture the same node, the earlier one wins.

; Literals that the grammar parses as plain identifiers.
((identifier) @constant
  (#any-of? @constant "true" "false" "null"))

; Comments.
(line_comment) @comment
(block_comment) @comment
(shebang) @comment

; Strings: only the pieces are captured, so code inside ${...} keeps its own colors.
(string_content) @string
(multiline_string_literal "\"\"\"" @string)
(string_literal "\"" @string)
(character_literal) @char
(escape_sequence) @escape
(interpolation "$" @punctuation.special)
(interpolation "${" @punctuation.special)
(interpolation "}" @punctuation.special)

; Numbers.
(number_literal) @number
(float_literal) @number

; Annotations and labels.
(annotation) @attribute
(file_annotation) @attribute
(label) @label
(return_expression label: (identifier) @label)
(this_expression (identifier) @label)
(super_expression (identifier) @label)

; Types.
(class_declaration name: (identifier) @type)
(object_declaration name: (identifier) @type)
(type_alias type: (identifier) @type)
(type_parameter (identifier) @type)
(user_type (identifier) @type)
(enum_entry (identifier) @constant)
(import (qualified_identifier (identifier) @type .)
  (#match? @type "^[A-Z]"))

; Functions: declarations, calls, constructor-like calls and infix calls.
(function_declaration name: (identifier) @function)
(call_expression . (identifier) @type
  (#match? @type "^[A-Z]"))
(call_expression . (identifier) @function)
(call_expression . (navigation_expression (identifier) @type .)
  (#match? @type "^[A-Z]"))
(call_expression . (navigation_expression (identifier) @function .))
(callable_reference (identifier) @function)
(infix_expression (_) . (identifier) @keyword.operator . (_))

; Keywords: declarations and modifiers.
[
  "package" "typealias" "class" "interface" "object" "enum" "fun" "val" "var" "constructor" "init"
  "companion" "by" "where" "get" "set" "field" "file" "property" "receiver" "param" "setparam" "delegate"
  "abstract" "actual" "annotation" "const" "crossinline" "data" "dynamic" "expect" "external" "final"
  "infix" "inline" "inner" "internal" "lateinit" "noinline" "open" "operator" "out" "override" "private"
  "protected" "public" "sealed" "suspend" "tailrec" "value" "vararg"
] @keyword

"import" @include

; Keywords: control flow, exceptions and expressions.
[ "if" "else" "when" ] @conditional
[ "for" "while" "do" ] @repeat
[ "try" "catch" "finally" "throw" ] @exception
[ "return" "return@" "this" "this@" "super" "super@" ] @keyword
[ "is" "!is" "in" "!in" "as" "as?" ] @keyword.operator
(reification_modifier) @keyword

; Operators.
[
  "+" "-" "*" "/" "%" "++" "--" "=" "+=" "-=" "*=" "/=" "%=" "==" "!=" "===" "!==" "<" "<=" ">" ">="
  "&&" "||" "!" "!!" "?:" "?" ".." "..<" "->" "::" "?."
] @operator

; Punctuation.
[ "(" ")" "[" "]" "{" "}" ] @punctuation.bracket
[ "," ";" ":" "." "@" ] @punctuation.delimiter
