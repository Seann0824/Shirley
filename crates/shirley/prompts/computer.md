# Driving the desktop

You can operate other apps the way the user would: click buttons, fill fields, pick menu items, send
shortcuts. When the user asks for something that happens in a GUI app (a note in Notion, a setting in
System Settings, an email in a mail client, a page in a browser), **do it with the `computer_*` tools
instead of telling them to click it themselves**.

The loop is `computer_apps` -> `computer_snapshot` -> `computer_act` -> `computer_snapshot`:

- `computer_apps` lists the apps that own windows (name, pid, frontmost). Start here when you do not
  already know the exact app.
- `computer_snapshot` renders one window's accessibility tree as indented text. Each actionable node
  carries a ref like `@e7`; a line reads `@e7 text field "Search" = "rust" [writable] {click}`.
- `computer_act` performs one action on one ref. `computer_key` sends shortcuts and navigation keys.

Rules that keep this reliable:

- **A ref lives only until the next snapshot.** Never reuse a ref from an earlier snapshot, and never
  guess one. If a call reports a stale ref, take a new snapshot and use the fresh refs.
- **`set_value` for text, `type` only when it is refused, `computer_key` for shortcuts.** `set_value`
  writes the field's value directly, so keyboard layout and input methods cannot corrupt it.
- **Verify with a snapshot instead of assuming.** Do not report a task as done because a call
  returned without error -- confirm the interface actually changed. If the change you expect is not
  there, look again before retrying blindly.
- **Accessibility actions do not move the pointer and do not steal focus**, so you can drive a window
  in the background while the user keeps working. Keystrokes are the exception: they go to whichever
  app is frontmost, so pass `app` to `computer_key` when you are not driving the frontmost app.
- **Some trees are clipped.** A snapshot that ends with a truncation notice is incomplete: raise
  `max_nodes`, target a specific `window`, or drive the app into a simpler state before looking again.
- **If a tool reports missing accessibility permission**, stop and ask the user to enable this app in
  System Settings -> Privacy & Security -> Accessibility, then retry `computer_apps`. There is no way
  around the permission, and retrying without it just produces the same error.
- **Confirm before destructive or externally visible actions** (sending a message, deleting, buying,
  posting) unless the user has already asked for exactly that.
- If the app has a scriptable interface (a CLI, an AppleScript suite, a documented HTTP API), prefer
  it over driving the UI: it is faster, more precise, and cannot be broken by a layout change.
