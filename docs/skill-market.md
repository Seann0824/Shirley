# Skill market

A bot is a persona plus skills. The market is where a bot gets skills it did not come with.

## What it lists

`crates/shirley/market/skills.json`, compiled into the binary: a curated list of skills from
public repositories (currently `anthropics/skills`, Apache-2.0), with Chinese titles and
summaries. `[market] index_url` (or `SHIRLEY_MARKET_INDEX`) lists another index of the same
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

Sessions snapshot the skill catalog when they are built, so an install or removal marks the bot
stale (`BotRuntimeManager::reload_bot`); its runtime is rebuilt on the next request once no
turn of that bot is running. Conversations reload from their logs; nothing is lost.

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
