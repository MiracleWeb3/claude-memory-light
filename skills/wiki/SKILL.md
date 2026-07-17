---
name: wiki
description: Karpathy-style personal wiki — curated, long-lived notes on decisions, incidents, architecture, and how-things-work, one topic per file. Use when a significant decision/incident/setup concludes and deserves a durable writeup, when the user says "add this to the wiki", or when a wiki page exists on the topic being discussed (check via cml search --role wiki).
---

# wiki

The wiki is the *curated* memory layer: notes a human (or you) would want to re-read months later. It lives at `~/.claude/claude-memory-light/wiki/` — plain markdown, Obsidian-compatible, indexed automatically by `cml index` (search with `cml search <terms> --role wiki`).

## The Karpathy model

One evolving page per topic — not a log, not append-only. When reality changes, EDIT the page so it stays true; the transcript history (cml's verbatim layer) already preserves how things looked before.

## Writing rules

- One topic per file, kebab-case filename (`vpn-routing.md`, `parser-architecture.md`, `incident-2026-07-lost-session.md`).
- Start with a one-line summary; keep the page self-contained and current.
- Link related pages with `[[wikilinks]]`. A link to a page that doesn't exist yet marks something worth writing.
- Good wiki material: decisions + their why, incident postmortems, non-obvious system setups, recurring procedures. Bad material: session chatter (cml has it), secrets (never).
- Before creating a page, `cml search <topic> --role wiki` — update the existing page instead of duplicating.

## Structural companion

If `graphify` is installed (`cml doctor` reports it), pair the wiki with a code knowledge graph: wiki explains *why*, the graph shows *what connects to what*.
