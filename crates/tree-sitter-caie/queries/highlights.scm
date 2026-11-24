(identifier) @variable
(number) @number
(string) @string
(character) @string
(boolean) @boolean
(comment) @comment
(primitive_type) @type
(operator) @operator
(assignment_operator) @operator
(parameter name: (identifier) @variable.parameter)
(constant_declaration name: (identifier) @constant)
(function_declaration name: (identifier) @function)
(procedure_declaration name: (identifier) @function)
(call_expression function: (identifier) @function)
(call_statement name: (identifier) @function)
[(declare_keyword) (constant_keyword) (output_keyword) (input_keyword)
 (if_keyword) (then_keyword) (else_keyword) (endif_keyword)
 (while_keyword) (endwhile_keyword) (repeat_keyword) (until_keyword)
 (for_keyword) (to_keyword) (step_keyword) (next_keyword)
 (case_keyword) (of_keyword) (otherwise_keyword) (endcase_keyword)
 (procedure_keyword) (endprocedure_keyword) (function_keyword)
 (endfunction_keyword) (returns_keyword) (return_keyword) (call_keyword)
 (array_keyword) (byref_keyword) (byval_keyword)] @keyword
["(" ")" "[" "]"] @punctuation.bracket
[":" ","] @punctuation.delimiter
"=" @operator
