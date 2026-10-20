# Global application storage

All Shirley runtime state lives under one global application directory:
`~/.shirley` (an explicit home path, identical across platforms and interfaces).
Desktop / TUI / cloud-server all share this single root; there is no per-interface
split. Source workspaces no longer contain an active `.shirley` directory.

```text
~/.shirley/
  config.toml                  # shared provider, memory, compression and runtime settings
  model-providers.json         # host-local provider profiles and model lists (includes credentials)
  remote.json                  # native cloud connection + optional personal-memory sync
  remote-sync-status.json      # sync status, no credential echo
  projects.json                # project registry
  memory/                      # personal / cross-project memory and vectors
  chat/                        # ordinary Chat working directory
  workspaces/<workspace-id>/
    workspace.json             # canonical source workspace path
    sessions/*.jsonl           # conversation logs
    sessions/*.title           # titles
    sessions/*.model.json      # provider/model binding, no credentials
    sessions/*.agents/         # subagent index and transcripts
    memory/                    # project memory and vectors
    legacy-config.toml         # archived former local config; never loaded as settings
    ...                        # other former .shirley files, including backups
```

`crates/shirley/src/storage.rs` owns path resolution and migration. The workspace id is UUID v5 over
the canonical path; identically named folders cannot collide, and opening a symlink to
the same workspace uses the same namespace. Source working directories and project
associations remain intact. Personal and project memory keep their semantic domains;
both are physically stored globally.

For cloud deployment, `~` is the home directory of the server process's OS user.
Keep that user's `~/.shirley` on persistent storage (mount it as a volume in a
container), and back up this whole directory. A separate server has its own data;
sharing the directory convention does not synchronize machines. Source projects
remain outside this state directory and must also be deployed or backed up as needed.
Workspace IDs depend on canonical absolute paths: copying state to a server with
different project paths does not automatically rebind existing sessions. Preserve
project paths when migrating until explicit path-rebinding support is implemented.

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

Startup also merges the former OS configuration root when `~/.shirley` already exists,
retaining current project registry entries and importing missing projects. The former
Chat workspace namespace is rebound to `~/.shirley/chat`, including session titles and
subagent files. Desktop and web both support this same Chat namespace.

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

Across different machines, only explicitly personal memory entries synchronize when enabled. Projects, project memories and sessions remain host-local; desktop can load remote sessions through the authenticated remote connection. See [memory-sync.md](./memory-sync.md).
