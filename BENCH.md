# cml benchmark history

Runner: `bench/run.sh` (median-of-N, own runner, no external harness — same ground rule
as nula/BENCH.md). Measures the **shipped Release binary end to end**, process start to
results printed, because that is the latency a caller actually pays.

## Method rules, learned expensively

1. **Never time a Debug build.** `cpp/CMakeLists.txt` makes `Debug` the *sanitizer* build
   (`-fsanitize=address,undefined`), roughly 10x slower. `bench/run.sh` refuses an
   instrumented binary outright (`nm` check for `__asan_`/`__ubsan_`).
2. **Pass the same flags to both arms.** See the 2026-08-07 note below — an A/B that
   flagged one side and not the other produced a 6.6x phantom gap.
3. **Record host contention, do not hide it.** The runner prints load average and marks
   `contended=yes` when load exceeds half the core count.

## Results

| date | benchmark | result | host |
|---|---|---|---|
| 2026-08-07 | `search --keyword` "touchpad drag", limit 20, median of 9 | 8.66 ms | contended, load 2.50 |
| 2026-08-07 | `search --keyword` "sanitizer build", limit 20, median of 9 | 13.88 ms | contended, load 2.50 |
| 2026-08-07 | `search --keyword` "embedding leg recall", limit 20, median of 9 | 5.12 ms | contended, load 2.50 |
| 2026-08-07 | `search --keyword` "rust c++ comparison", limit 20, median of 9 | 3.85 ms | contended, load 2.50 |
| 2026-08-07 | `search --keyword` "touchpad drag", limit 1..50, 30 runs each | 3.78–5.61 ms | load ~1.9 |

The 3.85–13.88 ms spread in the first four rows is host noise, not query cost — a 4-core
box with unrelated work running. **Quiet-host re-measurement pending.** The last row was
taken on a quieter host and is the better current estimate: **~4–5 ms** for a keyword
search over a 129 MB index, and wall time does **not** scale with `--limit`.

## 2026-08-07 — C++ vs the Rust predecessor: parity

cml was Rust through `952f53a` and C++ from `d3a2314` ("c++ port complete"). Both arms were
built Release and run against the same index, same queries:

| query | Rust `--keyword` | C++ `--keyword` |
|---|---|---|
| touchpad drag | 4.5 ms | 3.5 ms |
| sanitizer build | 5.5 ms | 4.0 ms |

**Result: parity, C++ marginally ahead.** The residual is below this method's noise floor
(~2 ms of shell fork overhead per iteration), so the honest statement is parity, not a winner.

**The trap that nearly wrote the opposite conclusion.** A first pass reported Rust 3.2–7.1x
faster. That measurement passed `--keyword` to the Rust arm and **not** to the C++ arm, while
being labelled "keyword-only BOTH SIDES". Without the flag the C++ default path runs the
vector rerank leg, loading potion-base-8M on every invocation:

| | no flag | `--keyword` |
|---|---|---|
| "touchpad drag", 20x | 30.0 ms | 3.5 ms |

~26 ms of model loading, attributed to the language. The claim survived only because no
benchmark existed to contradict it. That is why this file exists.

## Not yet measured

- Indexing throughput (`cml index`), the other half of the tool's cost.
- Semantic/hybrid search latency — only `--keyword` is covered here.
- Build time. Clean C++ build was measured once at 87 s versus 383 s for the Rust
  predecessor (214 transitive crates), but that is a one-off, not tracked here.
