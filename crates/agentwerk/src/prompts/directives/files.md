<!-- What the file tools say when a path, a match, or a write does not hold. The `path_hint_*` entries close a not-found message. -->

## edit_file_read_failed
{path} could not be read: {error}. Check the path with `list_directory` before editing it.

## edit_file_old_string_not_found
No `old_string` match in {path}. Read the file and copy the text exactly, including indentation, because `edit_file` matches byte for byte.

## edit_file_old_string_not_unique
`old_string` matches {count} places in {path}. Extend it with the surrounding lines until it is unique, or set `replace_all` to true to change every one.

## edit_file_write_failed
{path} could not be written: {error}. The file is unchanged.

## write_file_parent_not_created
The parent directories of {path} could not be created: {error}. Nothing was written.

## write_file_failed
{path} could not be written: {error}. Nothing was written.

## read_file_path_is_directory
'{path}' is a directory, not a file.

## read_file_path_is_directory_with_entries
'{path}' is a directory, not a file. Read one of its entries by appending the name to the path:
  {entries}

## read_file_is_binary
{path} is a binary file ({bytes} bytes), not text; it cannot be read as source. Judge from the information you already have.

## read_file_not_found
File does not exist: {path}. {hint}

## read_file_failed
{path} could not be read: {error}. Check the path with `list_directory` before retrying.

## list_directory_path_is_file
Path is not a directory: {path}. Read it with `read_file` instead.

## list_directory_not_found
Directory does not exist: {path}. {hint}

## list_directory_failed
{path} could not be listed: {error}.

## path_hint_directory_listed
'{dir}' contains:
  {entries}

## path_hint_suggestion
Note: your current working directory is {dir}. Did you mean {suggestion}?

## path_hint_working_directory
Note: your current working directory is {dir}.
