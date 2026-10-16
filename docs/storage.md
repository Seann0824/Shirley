# Global application storage

All Shirley runtime state lives under one global application directory:
`<config_dir>/shirley` (`~/Library/Application Support/shirley` on macOS).
Source workspaces no longer contain an active `.shirley` directory.

```text
shirley/
  config.toml                  # shared provider, memory, compression and runtime settings
  projects.json                # project registry
  memory/                      # personal / cross-project memory and vectors
  chat/                        # ordinary Chat working directory
  workspaces/<workspace-id>/
    workspace.json             # canonical source workspace path
    sessions/*.jsonl           # conversation logs
    sessions/*.title           # titles
    sessions/*.agents/         # subagent index and transcripts
    memory/                    # project memory and vectors
    legacy-config.toml         # archived former local config; never loaded as settings
    ...                        # other former .shirley files, including backups
```

`src/storage.rs` owns path resolution and migration. The workspace id is UUID v5 over
the canonical path; identically named folders cannot collide, and opening a symlink to
the same workspace uses the same namespace. Source working directories and project
associations remain intact. Personal and project memory keep their semantic domains;
both are physically stored globally.

Settings use `defaults < global config < explicit environment overrides`. Project
configuration is imported once during migration, never consulted as an override layer.
Existing global settings take precedence over imported values. `/login` writes the
global config. Already loaded runtimes retain their current settings until recreated;
restart after a global configuration change to apply it everywhere.

Migration runs before opening session handles, and on lazy project loading. Startup
visits the current workspace, ordinary Chat and registered projects (including archived
projects). Projects not registered yet migrate when opened. Missing/removed registered
directories are skipped; their global state is retained. `--migrate-data` performs only
this migration and exits. It does not query a model or curate memories.

A global file lock serializes migrations. Directory/file renames preserve existing
content; cross-filesystem moves copy and compare bytes before deleting originals.
Identical collisions are deduplicated; divergent collisions fail with both copies
preserved. Session names, titles, subagent links, memory provenance references and vector
model identifiers are kept. Legacy single-file `session.jsonl` is adopted by the session
catalog after moving into the global state directory.

Old application processes still use the previous paths. Exit and restart Shirley after
migrating; the new version consistently uses the global layout. The migration checks
run against isolated temporary application directories, never against real user data.

Manual desktop checks after restart: open an existing project and an old chat, inspect
its title/history and subagent transcript, then send a message. Expected: history is
available, the new message is saved under global workspace state, and the source repo
does not acquire a `.shirley` directory. Test `/login` in the TUI when changing provider
settings: expected persistence is the single global `config.toml`.
