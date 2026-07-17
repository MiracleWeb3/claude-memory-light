---
description: full-history memory — search past sessions, stats, doctor, reindex
argument-hint: search <terms> [--project P] [--role R] [--semantic|--keyword] | embed | map | stats | doctor | index [--all]
---

Run the claude-memory-light binary with the user's arguments:

```
cml $ARGUMENTS
```

Binary resolution: `cml` on PATH, else `~/.claude/claude-memory-light/bin/cml`, else bootstrap once via `"${CLAUDE_PLUGIN_ROOT}/bin/cml-run" $ARGUMENTS`.

Then:
- Show the output to the user in a code block, as-is.
- If it was a `search` with no hits, retry with 2-3 different keyword sets (synonyms, error text, filenames) before reporting not found.
- If the user wants a hit expanded, open the matching transcript `~/.claude/projects/<project-dir>/<session-prefix>*.jsonl` and quote the surrounding exchange.
- No arguments given → run `cml stats` and `cml doctor` and summarize both.
