(identifier) @variable
(number) @number
(string) @string
(character) @string
(boolean) @boolean
(comment) @comment
(lifetime) @attribute @lifetime
(type_identifier (identifier) @type)
(type_parameter name: (type_identifier (identifier) @type @type.parameter))
(parameter name: (identifier) @variable.parameter)
(field_declaration name: (identifier) @property)
(field_initializer name: (identifier) @property)
(member_expression name: (identifier) @property)
(scoped_identifier scope: (identifier) @type @namespace)
(scoped_identifier scope: (scoped_identifier name: (identifier) @type @namespace))
(module_declaration name: (_) @type @namespace)
(use_tree (identifier) @type @namespace)
(record_expression type: (_) @type)
(typed_collection type: (identifier) @type)
(function_declaration name: (identifier) @function)
(call_expression function: (identifier) @function)
(call_expression function: (scoped_identifier name: (_) @function))
(call_expression function: (member_expression name: (identifier) @function.method))
(generic_call_expression function: (identifier) @function)
(generic_call_expression function: (scoped_identifier name: (_) @function))
(attribute name: (identifier) @attribute)
(variable_declaration "const" name: (identifier) @constant)
(enum_variant name: (identifier) @constant)
(variant_pattern name: (identifier) @constant)
(variant_pattern name: (scoped_identifier name: (identifier) @constant))
(field_pattern name: (identifier) @property)
["let" "const" "fn" "proc" "gen" "record" "type" "enum" "match" "if" "else"
 "while" "for" "in" "return" "break" "continue" "object" "dynamic"
 "delete" "yield" "gc" "rc"] @keyword
(visibility ["pub" "crate"] @keyword)
(parameter "read" @keyword)
(variable_declaration "mut" @keyword)
(reference_type "mut" @keyword)
(unary_expression "mut" @keyword)
(captures "mut" @keyword)
(locality ["local" "shared"] @keyword)
(dyn_type "dyn" @keyword)
(use_declaration "use" @keyword)
(use_tree "as" @keyword)
(module_declaration "mod" @keyword)
(module_declaration "interface" @keyword)
(record_declaration "opaque" @keyword)
(enum_declaration "open" @keyword)
(interface_implementation ["impl" "interface"] @keyword)
(world_declaration "world" @keyword)
(world_include "include" @keyword)
(world_import ["import" "as"] @keyword)
(world_export "export" @keyword)
(trait_declaration "trait" @keyword)
(impl_declaration "impl" @keyword)
(property_declaration "property" @keyword)
(cast_expression "as" @keyword)
(yield_expression "from" @keyword)
["+" "-" "*" "/" "%" "=" "+=" "-=" "*=" "/=" "==" "!=" "<" ">" "<=" ">=" "&&" "||" "!" "&" "->" "=>" "|" ".." "..=" "..."] @operator
["(" ")" "[" "]" "{" "}"] @punctuation.bracket
[":" "::" "," ";" "."] @punctuation.delimiter
["#" "@"] @attribute
