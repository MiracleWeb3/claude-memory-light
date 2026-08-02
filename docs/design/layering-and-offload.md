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

**Verified on the real path (2026-08-02).** `strings` finds `updatedToolOutput` 13x in the
installed binary (`~/.local/share/claude/versions/2.1.220`). A first `grep -rl` over the same
file returned 0 for it — and 0 for `additionalContext`, which cml provably emits on every
prompt. That control is what exposed the method as confounded (263 MB ELF, bundle compressed)
rather than the field as missing.

The smoke test then ran twice, and the first run failed in the way that matters:

| run | replacement emitted | what the model saw |
|---|---|---|
| 1 | `"CML_PROBE_REPLACED_THIS"` (bare string) | the original 1..50 |
| 2 | `{"stdout": "CML_PROBE_REPLACED_THIS", "stderr": "", "interrupted": false, "isImage": false, "noOutputExpected": false}` | only the sentinel |

`updatedToolOutput` is **validated against the tool's own output schema and rejected in
silence** — the binary holds ``…does not match `${toolName}`'s output shape; using original
output``, but it never reaches stdout, stderr, or `--debug`. A wrong shape is
indistinguishable from a hook that never ran. Run 1 read exactly like the kill result and was
not one; taking it at face value would have ended the machine on a confounded test.

Run 2 also settled a question the spec had only hedged: **the replacement is what gets
persisted to the transcript.** The original output is gone from it entirely. The spill file is
therefore the only surviving copy, not a convenience.

**It only ever sees commands that succeeded.** `PostToolUse` does not fire on a failing Bash call —
measured live over five commands: `exit 2`, `exit 3`, and an `exit 1` printing to stderr all produced
no hook at all, while a benign non-zero (`grep` finding nothing) did. Confirmed independently by shape:
failing results carry `toolUseResult` as a **string**, not the `{stdout, stderr, …}` object, so
`offload()`'s `tool_response.stdout` lookup would passthrough even if the hook did fire. Of the 1,038
Bash results >= 2 KB in this machine's history, **zero** are failures; the 504 `is_error` results in the
corpus are all string-shaped.

So this does not rescue failing builds, and any framing that says it does is wrong. What it does is the
thing the spec actually set out to do — shrink large tool output — on the population where the bytes
are. The error rules still earn their place, because succeeding commands print diagnostics constantly:
across the corpus, 155 results contain `failed`, 50 `warning`, 28 a compiler-style `error:` and 8 a full
Python traceback, from the `cmd || true`, `2>&1 | tail` and read-a-logfile shapes an agent uses all day.

**Trigger.** Result >= 2 KB **and** `tool_name == "Bash"`. Nothing else, in v1.

Measured over the 146 transcripts, of the 11.60 MB carried by results at or above 2 KB:

| tool | results | MB | share |
|---|---:|---:|---:|
| Read | 1,057 | 6.23 | 53.7% |
| **Bash** | **1,029** | **4.09** | **35.2%** |
| other | 317 | 1.29 | 11.1% |

`Read` is the largest sink and is still never touched: at `PostToolUse` you have just read
that file *because you need it*. Condensing a `Read` would have to be retroactive, which this
hook cannot do — a real future direction, not this. `Grep`, `Glob` and MCP wait until their
result shapes are recorded the way Bash's was; guessing a shape yields a feature that does
nothing and reports success. So Bash is the whole reachable win: **35.2% of the >= 2 KB class,
or 22.2% of all tool bytes.**

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

  Measured with scenes as a **re-rank over the same `mem` rowids**, not as a replacement
  result. `eval.cpp:109` matches `hits[i].rowid` against the answer row; a scene line
  occupying one of the three slots would cut the matchable slots to two and depress the
  number whatever the layering did. The scene headline is presentation and is added in
  `recall()` only after this gate passes.
- **Offload:** measured tool bytes reaching context, before and after, over real sessions —
  not a selftest, not a synthetic transcript. Target: **Bash results >= 2 KB shrink by at
  least half**, which on the measured distribution is ~11% of all tool bytes.

  **Measured before wiring: 48.2%** over 1,046 real Bash results (4.17 MB -> 2.16 MB), so the
  target is missed by 1.8 points — ~10.5% of all tool bytes rather than ~11%. The prediction
  of what a half would be worth was sound; the half is what the rules do not reach. See
  Calibration below for the curve, and for the 53.0% that is available with the drag passes
  off at 38% leave-one-out survival. Whether that trade is worth taking is a decision, not a
  knob to turn until the number clears.

  That is the honest ceiling for v1 and it is well short of the 61% headline that motivated
  this — most of which came from their layering, and most of the rest of the reachable bytes
  here sit in `Read`, which is deliberately out of scope. A target set against the whole
  >= 2 KB class (~31%) would have been unreachable by construction, and hitting "only" 11%
  would have read as failure against a number that was never possible.
- **No silent loss:** for every offloaded result, the spill file exists and its content
  matches what the tool actually printed. Asserted in selftest.
- Recall on a prompt matching no scene returns exactly what it returns today.

## Calibration, before either full run

`docs/design/durability-gate.md` records the method that saved an hour there: the first
rubric minted 80% and was killed two minutes in. Same discipline applies twice here.

- **Scene rubric:** calibrate on 20 sessions before running all 75. A scene summary that reads
  as a status report ("worked on the parser") is the failure mode to catch, and it is
  exactly what the durability gate found the judge doing at low strictness.
- **Condenser: measured, and it misses the gate.** Run over the real results already on disk
  **before** it is ever wired to the hook — `cpp/tools/calibrate-offload.cpp`, 1,046 Bash
  results >= 2 KB, 4.17 MB. (Not 2,438: that count is every tool, and 1,059 of those are
  `Read`, which `condensable()` never accepts.)

  The first gate here was "count error lines before and after; anything below 100% is a bug."
  That gate is a **tautology** — every needle match is kept unconditionally, so it reads 100%
  for every possible input. Measured: a sabotaged condenser keeping *only* needle lines and
  discarding every traceback body scored 92.3% saved / 0 lost, beating the real one's 44.4%.
  It is replaced by two numbers the needle list cannot produce about itself:

  **Leave-one-needle-out.** For needle *k*, take the lines matching *k* and no other needle —
  their survival depends entirely on *k* — then condense with *k* disabled and count how many
  survive on head/tail and the drag passes. That is a diagnostic line the needle list cannot
  see, simulated 31 ways over real data, and it moves when the rules move.

  **Per-file differential.** Condense the corpus with two builds and sort by byte delta. A
  global rate cannot move on a 9-in-1,046 regression; this shows it in a two-minute read.

  | | bytes saved | leave-one-out survival |
  |:--|--:|--:|
  | both drag passes off | **53.0%** | 38.0% |
  | downward drag only (`kDragUp = 0`) | 49.4% | 48.3% |
  | **shipped, `kDragUp = 1`** | **48.2%** | **53.3%** |
  | `kDragUp = 6` (the unevidenced original) | 44.4% | 59.4% |
  | `kDragUp = 12` | 41.9% | 62.6% |

  Both curves are monotone and **opposed**, so no depth clears both gates: bytes >= 50% needs
  the drag rules off, survival >= 70% is unreachable at any depth. The rules are what costs
  the byte gate — 4.8 points — and what buys 15 points of survival. `kDragUp = 1` is the knee:
  1.2 points of bytes for 5.0 of survival, where every later step buys 1-2.

  Read the survival figure with its population in view. The 70% bar was set against a looser
  one — *any* line matching *k*, of which 798 of 2,678 also match another needle and are kept
  unconditionally whatever *k* does. On that population the same binary reads 67.2% at depth 1
  and 71.5% at depth 6, and 29.8% of it is the free pass the killed gate was made of.

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
