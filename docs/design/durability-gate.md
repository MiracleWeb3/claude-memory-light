# cml: the durability gate

**Date:** 2026-07-26
**Status:** approved, implementing

## The problem

`cml map` plotted 1,152 points and read as "1,152 things you know." The owner's estimate of
what is actually valuable was 200–250. Measurement across the session says he was right:

| layer | count |
|---|---|
| rows in the index | 1,155 |
| minus memory/wiki file-rows | 988 conversation rows |
| rows carrying a gist | 500 |
| distinct after collapsing near-identical gists | 487 |
| ...of which roughly half read as durable | **~250** |

The map has since been changed to plot only gist-bearing rows plus curated notes (667), but
the gists themselves were minted under a lenient bar. Half of them are tasks or status —
`'Ensure mouse settings persist after reboot.'` sits beside
`'To bypass X E2EE passcode, visit /i/chat root first, dismiss cookie banner, then navigate to thread.'`

## Why not just delete more

Tried and measured. A strict row-level keep/drop rubric dropped 121 assistant rows, and
**37% of them carried durable knowledge buried behind a status opener** — the rezka CDN
diagnosis, the origin of skill-matcher. The judge reads the first sentence. It was also
unstable: two passes dropped 159 and 196 rows while agreeing on only 121, and it both kept
and dropped identical duplicate text.

Root cause: assistant messages **mix status and knowledge in one row**. Keep/drop cannot
resolve a row that is half completion report and half mechanism. Extraction can — and
`gist` already extracts. It is currently a byproduct; it should be the gate.

## Design: two independent axes

| axis | question | cost of a wrong answer | strictness |
|---|---|---|---|
| `keep` (exists) | is there *any* content here? | knowledge lost forever | lenient, unchanged |
| `durable` (new) | would this save real time months from now, on a different problem? | none — the row stays searchable | strict |

The asymmetry is the design. A wrong "not durable" costs nothing because the row remains in
`mem` and BM25 still finds it; it simply is not plotted as a fact. So the durability bar can
be brutal in a way the deletion bar never could.

### The mechanism already exists

No schema change is needed. Today, zero rows exercise this path:

- `db.cpp:145` — `gist_lookup` is `SELECT key, gist FROM distilled WHERE gist != ''`
- `distill.cpp` — the "already judged" set is `SELECT key FROM distilled`

Therefore a row written to `distilled` with an **empty gist** is simultaneously:
judged (never re-judged), searchable (still in `mem`), and unplotted (skipped by
`gist_lookup`). That is exactly the "kept but not durable" state, already supported.

### Changes

1. **`curatorprompts.cpp`** — both rubrics gain a durability clause: `keep` semantics are
   unchanged, but a gist is emitted **only** when the row states a durable fact; otherwise
   `gist` is the empty string. The strict bar: mechanisms, root causes, gotchas that would
   cost hours to rediscover, decisions with their reasons, standing rules, and durable facts
   about the user's own environment. Excluded: completion reports, mode/config
   acknowledgments, status, plans, and answers to one-off questions.
2. **`report.cpp`** — `cml stats` reports the knowledge count next to the row count, so the
   two numbers can never be confused again.
3. **`selftest/curate.cpp`** — assertions locking the two-axis contract, mutation-checked.
4. **Re-mint** — `cml distill --all` re-judges the 501 existing gists under the new bar.
   `--all` judges ~560 rows per run, so this takes several runs to drain.

### Non-goals

- No deletion. This design removes nothing.
- No near-duplicate collapsing (50 rows, deterministic, separate concern).
- No change to the `keep` rubrics' drop behaviour, including the user rubric's
  correction guard.

## Success criteria

- `cml map` plots roughly 250 knowledge points, down from 667. Knowledge is
  `gists + curated notes`, and the notes are a fixed 167, so the target implies roughly
  **85 durable gists out of ~988 judged candidates — about a 9% mint rate**. If the rate
  lands far above that the bar is still too soft; far below and the map is emptier than the
  notes alone, which means the clause is being read as "gist almost nothing".
- Every row currently in the index is still returned by `cml search`.
- Spot-check: `'Ensure mouse settings persist after reboot.'` loses its gist;
  `'To bypass X E2EE passcode...'` keeps one.
- Selftest passes, and removing the durability clause from a prompt makes it fail.

## Calibration (done — the bar was measured, not guessed)

The first durability clause listed "a lasting fact about the user's own environment, paths, or
configuration" as durable and minted **80%** of rows. The model was obeying it correctly; the
clause was wrong, because nearly every message is a true fact about his machine. It produced
gists like `'XFCE screenshooter shortcuts: Print for full screen'` — true, useless.

Each test was then added and re-measured on a fixed 100-row sample:

| clause | mint rate | projected points |
|---|---|---|
| v1 — "lasting fact about the environment" | 80% | 957 |
| v2 — + five-minute test | 30% | 466 |
| **v3 — + decision test + cost test** | **19%** | **354** |

v3 ships. Landing at 354 rather than 250 is deliberate: at 19% the sample was still minting
things like `"skill matcher silently fails past ~50 skills because no error is thrown"` and
`"Dashboard is a read-only mirror; it never runs the parser"`. Halving again would cut those.

The 354 splits as **187 discovered gists + 167 curated notes**. The owner's "200–250 actually
valuable" maps onto the gist count, which lands inside it; the notes are his own memory files
and were never in question.

**Method note:** calibrate a rubric on a 100-row sample before running it over the corpus. The
first full run was killed two minutes in once the 80% rate was visible, which cost two minutes
instead of an hour.

## Risks

- **The judge is unstable at high strictness** (measured: 159 vs 196 on the same input).
  Mitigated by the asymmetry — an unstable durability verdict costs nothing, unlike an
  unstable deletion verdict. Not worth a two-pass intersection here.
- **Re-minting is not free** (~1,500 API calls). Reversible by re-running, since no row is
  destroyed.
