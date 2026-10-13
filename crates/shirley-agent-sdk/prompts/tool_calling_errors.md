# Correctable parse-error messages for the Content tool-calling protocol.
# Each line is `key=template`; placeholders: {{tool}} / {{name}} / {{params}} /
# {{description}} are the current protocol's tag names; {{tool_name}} is the
# invoked tool's name. Editing the
# text requires no code change; for a non-English locale, ship another template and swap
# the include_str! source.
missing_name=The tool block is missing `<{{name}}>`
invalid_params=`<{{params}}>` of tool `{{tool_name}}` is not valid JSON
truncated_params=The tool block was truncated (`<{{params}}>` is unclosed) and cannot be executed
leading_text=The tool call was preceded by plain text, so no tool ran. Resend the full reply starting with `<{{tool}}>` or `<{{description}}>`; keep all prose inside protocol tags.
unclosed_tool=The tool block is unclosed (missing `</{{tool}}>`)
