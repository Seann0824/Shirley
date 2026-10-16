# Workspace grep tool

`grep` is an application-layer content search tool registered for both parent and child
agents. It uses ripgrep's `ignore` crate plus `regex` and `globset`; no external `rg`
installation or shell command is needed.

The default traversal honors nested `.gitignore`, `.ignore`, `.rgignore`, Git excludes
and global Git ignores. `.gitignore` also works outside a Git repository. A search always
starts its ignore-aware walk at the workspace root: supplying an ignored file/directory
as `path` does not bypass ignores. `glob` is an additional intersection filter, rather
than a ripgrep-style glob override that could re-include ignored files. Hidden entries
are skipped by default. Set `include_ignored=true` or `include_hidden=true` explicitly
when the task requires them; these controls are independent.

The tool accepts `pattern` (single-line regex by default), `path`, `glob`,
`case_insensitive`, `fixed_strings`, `files_only`, `max_results`, `include_ignored` and
`include_hidden`. Paths/globs are relative to the workspace root. Content results use
`path:line:text`; `files_only=true` returns each matching file once. Use `read` to
inspect the surrounding lines.

```json
{"pattern":"IndexRecord::Started|dispatch\\(|SubagentInfo \\{","glob":"*.rs","max_results":40}
```

Ignore rules are search preferences, not an output-size boundary. The following limits
remain in force even when ignored/hidden files are enabled:

- Default 100 matching lines/files; requested limits are capped at 500.
- At most 32 KiB of returned text, including status notices; long-line previews are
  limited to 1000 bytes. Matching still examines the full line in a bounded file.
- Files over 2 MiB are skipped and reported, including files growing during a read.
- At most 50,000 yielded traversal entries and a cooperative 10-second scan deadline.
  Blocking filesystem calls cannot be preempted by this deadline.
- NUL-containing files, symbolic links, suspected secret filenames (same policy as
  `read`), VCS directories and `.shirley` state are always skipped. An explicitly
  requested symlink is resolved using the existing workspace boundary check.
- Glob/regex/path validation errors are tool errors. Traversal/ignore/read errors are
  counted in a partial-search notice instead of silently reporting a complete search.

Traversal and matching run on a blocking worker, keeping the async executor available.
No network access or command execution is involved. The bounded bash capture remains
as a fallback for commands that still produce oversized output.
