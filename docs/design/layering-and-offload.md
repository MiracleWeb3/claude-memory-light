# cml: layered recall and tool-output offload

**Date:** 2026-08-02
**Status:** approved, implementing

Both ideas are taken from [TencentDB-Agent-Memory](https://github.com/TencentCloud/TencentDB-Agent-Memory)
(MIT, 10.7k stars). That project cannot be installed here — it is a plugin for OpenClaw and
Hermes Agent, mentions Claude Code nowhere in its 580-line README, requires Node >=22.16 and a
mandatory `MODEL_API_KEY`, and runs a Gateway process besides. Nothing of its code is reused.
What is taken is two design ideas it demonstrates and cml lacks.

## The problem

**Recall is flat.** `recall.cpp` ranks rows and returns three of them (`kMaxHits = 3`,
`recall.cpp:41`). Every hit is an independent message. Three rows from one debugging session
arrive as three unrelated lines, and three rows from three different months arrive looking
identical to that. Measured: recall@3 is **19.7%** (`cml eval`, 2026-07-28).

Against the four layers TencentDB builds, cml has:

| layer | cml today | source |
|---|---|---|
| L0 raw | 2,786 `mem` rows + 36,795 `work` rows | indexed |
| L1 atoms | 178 gists — each from **one** message, capped at 500 chars | `distill.cpp:82` |
| L2 scenario | **nothing** | — |
| L3 persona | `MEMORY.md` / `CLAUDE.md`, hand-written | indexed as `role='memory'` |

L3 already exists and is the owner's own standing orders. L2 is the hole: **not one session in
the index is summarized.**

How many sessions is a scene actually worth? Measured, because the first three counts to hand
disagreed — `cml stats` says 121, `count(DISTINCT session)` says 305, and there are 146
transcript files:

| session size | count |
|---|---|
| 1 row | 205 |
| 2-4 rows | 25 |
| 5-19 rows | 38 |
| 20+ rows | 37 |

**205 of 305 sessions hold a single row.** A scene over one message restates that message.
So scenes are built only for sessions with **>= 5 rows: 75 of them.**

**Nothing manages in-session context.** cml is entirely a between-sessions tool. Tool output
enters the context window at full size and stays there until compaction throws it away.
Measured across the 146 raw transcripts on this machine:

| | |
|---|---|
| tool results | 19,662 · 18.3 MB |
| median | 239 bytes |
| p90 / p99 / max | 2,438 / 9,556 / 61,658 |
| **>= 2 KB** | **12.4% of results, 63.1% of all tool bytes** |

One call in eight carries two thirds of the bytes. That is the leverage.

## Why not just do what they did

Their L3 is LLM-generated. Here it is not built at all. `MEMORY.md` and `CLAUDE.md` are the
owner's standing orders, written by hand; a generated profile that drifts from them, or
quietly contradicts them, would be invisible until it caused a wrong action. The cost of a
wrong L3 is unbounded and the cost of not having one is zero, because the layer already
exists in better form. **Non-goal, permanently.**

Their symbolic layer condenses tool logs into Mermaid diagrams. A counted line is smaller
than a diagram of a build log and needs no renderer. Not copied.

## Machine A: `cml offload`

`PostToolUse` -> `cml offload` -> `hookSpecificOutput.updatedToolOutput`, which replaces the
tool result before it reaches the model.

**Verified, not assumed.** `strings` finds `updatedToolOutput` 13x in the installed binary
(`~/.local/share/claude/versions/2.1.220`). A first `grep -rl` over the same file returned 0
for it — and 0 for `additionalContext`, which cml provably emits on every prompt. That
control is what exposed the method as confounded (263 MB ELF, bundle compressed) rather than
the field as missing. Implementation still starts with a live smoke test; a docs claim and a
string in a binary are not a firing hook.

**Trigger.** Result >= 2 KB **and** the tool is on an explicit allowlist: `Bash`, `Grep`,
`Glob`, and any name starting `mcp__`. An allowlist, not a denylist — an unknown tool is left
alone, so a tool added by a future version is never silently condensed. Never `Read`, `Edit`,
`Write`, `NotebookEdit`: that is the file being worked on, and shrinking it is how the model
loses the thing it is editing.

**Condenser — deterministic, zero LLM.** A hook that blocks on a network call stalls every
tool call in the session. Kept verbatim: lines matching
`error|fail|panic|Traceback|undefined reference|fatal|warning`, the first 5 lines, the last
15. Everything else collapses to one line:

```
<cml: 412 lines elided · 0 errors · 38 warnings · full: ~/.claude/claude-memory-light/spill/<session>/<n>.txt>
```

**Lossless, via a spill file — not via `work`.** `work` truncates at `kToolMax = 1200`
(`transcript.cpp:61`), and that cap is correct: its comment argues "a megabyte of source is
not a memory," and the search index agrees. It is wrong only for recovery. So `cml offload`
writes the untouched original to a spill file before emitting the replacement, and names that
path in the symbol line. No schema change, no index bloat, and recovery is `cat` — reachable
by the model and by hand, needing no new cml subcommand.

The recovery pointer is a backstop, not the design. Per the 2%-invocation rule, anything that
depends on the model choosing to expand it will mostly not happen — so the condenser must
keep the decision-relevant bytes verbatim in the first place. That is what the error-line
rule is for.

## Machine B: L2 scenes and layered recall

**Schema.** New table `scene`, one row per session: `session` (pk), `project`, `ts_start`,
`ts_end`, `n_rows`, `title`, `summary`, `outcome`, plus an FTS5 index over
`title|summary|outcome`.

**Built by** `cml distill --scenes`, through the existing `judge_batch` path with a new
`Rubric::Scene`. Input per session: its kept rows, gist where one was minted, else truncated
text. Output: title, 2-3 sentence summary, outcome in {solved, abandoned, ongoing}.
Only sessions with `n_rows >= 5` — **75 calls**, not 305, on the DeepSeek key already
configured (`distill.cpp:29`). The 205 single-row sessions are left at L0, where they already
work: one row needs no summary to be found.

**Layered recall.** The injection budget stays three lines — it is seen on every prompt and
growing it is how a briefing becomes wallpaper. What changes is what fills it:

1. rank `scene` with the same query, through the existing rarity and overlap gates
2. a scene wins -> **1 scene line + up to 2 atoms drawn from that same session**
3. no scene -> today's path, unchanged

That is progressive disclosure done push-style: the hook picks the depth. A pull-based
drill-down was rejected for the same reason recall itself is hooked — `recall.cpp:1-7`
records the read half firing 2% of the time when it was the model's decision to make.

L3 needs no work. `role='memory'` rows are already indexed and already reachable, so the
drill order is standing rules -> episode -> the message itself.

## Non-goals

- Auto-generated L3 persona. Permanently, for the reason above.
- Mermaid symbols.
- Team, ACL, sharing, governance — this is one machine with one user.
- Any new process, daemon, port, container, or Node dependency.
- Raising `kToolMax`. The index cap is correct; the spill file is a separate concern.

## Success criteria

Both halves are gated on a measured number, not on shipping.

- **Scenes:** `cml eval` recall@3 must exceed **19.7%**. If it does not move, the layer is
  decoration and gets deleted rather than defended.
- **Offload:** measured tool bytes reaching context, before and after, over real sessions —
  not a selftest, not a synthetic transcript. Target: the >= 2 KB class shrinks by at least
  half, which on the measured distribution is ~31% of all tool bytes.
- **No silent loss:** for every offloaded result, the spill file exists and its content
  matches what the tool actually printed. Asserted in selftest.
- Recall on a prompt matching no scene returns exactly what it returns today.

## Calibration, before either full run

`docs/design/durability-gate.md` records the method that saved an hour there: the first
rubric minted 80% and was killed two minutes in. Same discipline applies twice here.

- **Scene rubric:** calibrate on 20 sessions before running all 75. A scene summary that reads
  as a status report ("worked on the parser") is the failure mode to catch, and it is
  exactly what the durability gate found the judge doing at low strictness.
- **Condenser:** run over the 2,438 real results >= 2 KB already on disk and diff the
  condensed form against the original **before** it is ever wired to the hook. The question
  is not "does it shrink" but "does the kept text still contain the error." Measure the
  fraction of results whose error lines survive; anything below 100% is a bug in the rule,
  not an acceptable rate.

## Risks

- **Offload removes something the model needed, mid-task.** Bounded by the tool allowlist and
  the 2 KB floor: 87.6% of tool calls are never touched. Recoverable via the spill file. The
  residual risk is a Bash result whose useful content is neither an error line, nor in the
  first 5, nor in the last 15 — a long informational listing. Accepted; the alternative is
  not shipping.
- **`updatedToolOutput` behaves differently than documented** — e.g. the transcript on disk
  keeps the original, or the replacement is ignored. The smoke test is step 1 precisely
  because everything else is built on it. If it does not fire, Machine A stops there and is
  not built.
- **Scenes cost 75 API calls** and buy nothing measurable. Mitigated by calibrating on 20
  first, and by the recall@3 gate, which is what decides whether they stay.
- **Session is the wrong unit for L2.** A long session covers several unrelated problems; 37
  sessions here hold 20+ rows and the largest holds 282. Sub-session episode splitting is
  deliberately deferred — one scene per session is the lazy version, and the gate will show
  whether it is enough. If recall@3 moves but only on short sessions, that is the signal to
  split.
