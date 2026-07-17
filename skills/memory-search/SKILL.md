---
name: memory-search
description: Full-history recall over ALL past Claude Code sessions, curated memory files, and the wiki (local FTS5 index, zero tokens). Use whenever the user references past sessions, prior decisions, or work not in current context — "we discussed", "last time", "you already did/solved", "when did we", "find that session about X" — or before ever claiming you lack context on prior work.
---

# memory-search (cml)

`cml` indexes every Claude Code transcript (`~/.claude/projects/*/*.jsonl`), every curated memory file (`~/.claude/projects/*/memory/*.md`), and the wiki (`~/.claude/claude-memory-light/wiki/*.md`) into one local SQLite FTS5 database. Search costs zero LLM tokens and returns in milliseconds. A Stop hook keeps the index current automatically.

Binary resolution: `cml` on PATH, or `~/.claude/claude-memory-light/bin/cml` (the plugin's `bin/cml-run` bootstraps it on first use).

## Recall protocol

1. `cml search "<keywords>" [--project <substr>] [--role user|assistant|summary|memory|wiki] [--limit N]`
   - Hybrid by default: FTS5 keywords + local semantic vectors, rank-fused. Semantic recall means paraphrases hit too — describe the CONCEPT if exact words are unknown.
   - `--keyword` forces exact FTS5 only; `--semantic` forces vectors only. If it reports the semantic index is missing, run `cml embed` once (downloads a 30 MB local model, no API).
   - No hits ≠ not found: try 2-3 DIFFERENT phrasings before concluding anything.
2. Full context around a transcript hit: open `~/.claude/projects/<project-dir>/<session>*.jsonl` (glob the 8-char session prefix) and grep near the snippet text.
3. `cml stats` — coverage. `cml index` — manual refresh. `cml index --all` — full rebuild (index is disposable; transcripts are the source of truth).
4. `cml map` — render the whole memory as an interactive 3D graph (one offline HTML file, opens in browser). Use when the user asks to "see", "visualize", or "map" their memory.

## Rules

- Run `cml search` BEFORE re-solving anything that smells solved before, and before saying "I don't have context on that".
- Prefer it over grepping raw transcripts — FTS5 is ranked and instant.
- `--role memory` searches only curated memory notes; `--role wiki` only wiki pages; default spans everything.
- Division of labor: curated memory = distilled facts; cml = verbatim full history. Recall questions get both.
