<!-- One line per schema violation, kept terse: a rejected value can carry twenty of them in one report. The `schema_hint_*` entries close a type violation. -->

## schema_false_rejected
value is rejected by `false` schema

## schema_type_mismatched
expected type {expected}, got {got}

## schema_const_mismatched
expected {expected}

## schema_enum_mismatched
value is not in `enum`

## schema_any_of_unmatched
value does not match any of the anyOf schemas

## schema_one_of_ambiguous
value matches {count} of the oneOf schemas, expected exactly 1

## schema_not_matched
value must not match the `not` schema

## schema_property_missing
missing required property `{name}`

## schema_property_unexpected
unexpected property `{name}`

## schema_array_too_short
array has {count} items, expected at least {min}

## schema_array_too_long
array has {count} items, expected at most {max}

## schema_string_too_short
string length {length} is below minimum {min}

## schema_string_too_long
string length {length} is above maximum {max}

## schema_pattern_unmatched
string does not match pattern `{pattern}`

## schema_number_too_small
value {value} is below minimum {min}

## schema_number_too_large
value {value} is above maximum {max}

## schema_hint_unquote
send the value unquoted

## schema_hint_json
send it as JSON, not as a string

## schema_hint_quote
send the value quoted
