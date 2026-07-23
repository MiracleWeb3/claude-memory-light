---
name: learning-loop
description: Hermes-style continuous learning across sessions. Use when a SessionStart nudge reports accumulated learning signals, when the user corrects you or hands you a working method worth keeping, or when asked to consolidate lessons/memory. Turns raw per-turn signals into durable memory that conditions every future session.
---

# learning-loop

Frozen model weights can't learn — but persistent memory files loaded each session are behaviorally a fine-tune. This loop makes learning compound:

1. **Capture (automatic).** A Stop hook (`cml capture`) appends each turn's user message to a per-project inbox at `~/.claude/claude-memory-light/inbox/<project>.md`, flagged `(correction?)` when it looks like a correction, `(note)` otherwise. No LLM calls — the intelligence lives in consolidation, where full context already exists.
2. **Nudge (automatic).** A SessionStart hook (`cml nudge`) injects a reminder once ≥5 signals pile up (tune with `CML_NUDGE_THRESHOLD`).
3. **Consolidate (you, when nudged).** Early in the session, before deep work:
   - Read the inbox file the nudge names.
   - Distill durable items — corrections, preferences, working methods, non-obvious workflows — into your persistent memory (auto-memory files, or the user's convention). Record the user's words for orders/preferences, not a paraphrase.
   - **Correction-sweep:** when consolidating a correction, run `cml search "<key terms>" --role wiki` and `cml search "<key terms>" --role memory` for prior restatements of the now-stale claim — fix or flag every hit in the same pass. A correction that leaves old restatements standing hasn't corrected anything.
   - **Promote what recurs:** a lesson seen ≥2 times graduates into the always-loaded CLAUDE.md; a rule shaped "always do X before Y" graduates further into a hook or skill.
   - Delete the consolidated lines from the inbox. Drop the noise — most lines are nothing.

## Rules

- Capture beats recall: also write memory at the moment of a correction, not only at consolidation time.
- Wrong memories get deleted, not accumulated — newest information wins.
- Never store secrets (tokens, passwords) in memory files or the inbox.
