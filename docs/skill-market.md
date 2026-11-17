# Skill market

A bot is a persona plus skills. The market is where a bot gets skills it did not come with.

## What it lists

`crates/shirley/market/skills.json`, compiled into the binary: a curated list of skills from
public repositories, with Chinese titles and summaries:

| Source | Licence | Skills |
| --- | --- | --- |
| `anthropics/skills` | Apache-2.0 | documents (pdf, docx, xlsx, pptx), design, MCP, skill writing |
| `obra/superpowers` | MIT | working methods: brainstorming, plans, debugging, TDD, verification |
| `openai/skills` (`.curated`) | Apache-2.0 per skill | Jupyter, Playwright, security reviews, GitHub CI and PR comments, CLIs |
| `huggingface/skills` | Apache-2.0 | papers, datasets, running models locally | `[market] index_url` (or `SHIRLEY_MARKET_INDEX`) lists another index of the same
shape instead; when it cannot be fetched, the built-in list is used.

```json
{ "skills": [ { "id": "pdf", "title": "PDF 处理", "summary": "...", "category": "文档",
                "repo": "anthropics/skills", "ref": "main", "path": "skills/pdf",
                "license": "Apache-2.0" } ] }
```

`id` is the directory the skill installs as, which is also its name (`SKILL.md` names come
from the directory). Entries whose id, repo or path could escape their directory are dropped.

## Where a skill goes

Into one bot: `~/.shirley/bots/<bot>/skills/<id>/`, with a `.market.json` recording the source.
Only that bot sees it. A skill a bot writes for itself goes in the same directory without the
marker; it is listed on the bot's skills page but never offered to the market (bot-written
skills are not stable enough to share).

Sessions snapshot the skill catalog when they are built, so a change to the directory (an
install, a removal, a skill the bot saved) makes the runtime stale; it is rebuilt on the next
request once no turn of that bot is running. Conversations reload from their logs; nothing is
lost.

## Skills a bot writes itself

A bot saves a procedure it will need again with the `save_skill` tool (name, one-line
description, Markdown instructions, optional files under `scripts/`, `references/`,
`assets/`). It lands in the same `bots/<bot>/skills/` directory without a market marker, so it
shows on the skills page as "Bot 自己写的" and is never offered to the market. Saving under the
same name replaces it; a market skill's name is refused (the next install would overwrite it).
The bot is told when to do this in `prompts/skills_own.md`, part of its skill listing.

The skill can be activated in the same turn (activation re-scans on an unknown name). The
runtime manager fingerprints each bot's skill directory on every request and rebuilds an idle
runtime when it changed, so it is listed from the next message on -- the same path picks up
market installs and skills edited by hand.

## How it is fetched

File by file, so the server needs neither git nor archive tools:

1. File list: GitHub's tree API, else jsDelivr's package listing.
2. Each file: `raw.githubusercontent.com`, else the jsDelivr CDN (usually reachable where GitHub
   is slow or blocked).

At most 400 files and 40 MB per skill. Files land in a hidden `.installing-*` directory and only
become the skill once all of them arrived and `SKILL.md` parses.

## Creating a bot

The "+" page sends the description to `agent_design_bot`: one model call
(`prompts/design_bot.md`) returns an id, a display name, a persona and the market skills the
bot needs. The spec is saved and the bot's conversation opens right away; the skills download
in the background and the bot picks them up when they arrive.

## Commands

| Command | Arguments | Returns |
| --- | --- | --- |
| `agent_design_bot` | `brief` | `{ bot, display_name }` |
| `agent_skill_market` | `bot` | `{ market: MarketSkill[], installed: InstalledSkill[] }` |
| `agent_skill_install` | `bot`, `id` | 204 once installed |
| `agent_skill_uninstall` | `bot`, `id` | 204 |
