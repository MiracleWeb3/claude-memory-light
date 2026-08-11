<a name="top"></a>
<div align="center">

<img src="https://capsule-render.vercel.app/api?type=waving&height=180&color=gradient&customColorList=12&text=claude-memory-light&fontSize=44&fontColor=ffffff&animation=fadeIn&fontAlignY=36&desc=full%20memory%20for%20Claude%20Code&descSize=18&descAlignY=56" width="100%" alt=""/>

<img src="assets/logo.svg" width="150" alt="cml logo"/>

<br/>

<img src="https://readme-typing-svg.demolab.com/?font=Fira+Code&size=17&pause=1400&center=true&vCenter=true&width=560&color=EA580C&lines=every+session+already+on+disk%2C+indexed;the+read+half%2C+hooked+and+measured;0+tokens+·+0+daemons+·+1+small+Rust+binary" alt=""/>

<br/>

[![build](https://img.shields.io/github/actions/workflow/status/MiracleWeb3/claude-memory-light/release.yml?style=for-the-badge&logo=githubactions&logoColor=white&label=build)](https://github.com/MiracleWeb3/claude-memory-light/actions)
[![release](https://img.shields.io/badge/release-v3.0.0-ea580c?style=for-the-badge&logo=github)](https://github.com/MiracleWeb3/claude-memory-light/releases)
[![license](https://img.shields.io/badge/license-MIT-blue?style=for-the-badge)](LICENSE)
[![rust](https://img.shields.io/badge/rust-2021-dea584?style=for-the-badge&logo=rust&logoColor=white)](https://www.rust-lang.org)


**[install](#install)** · **[use](#use)** · **[how it works](#how-it-works)** · **[recall](#recall--the-read-half)** · **[learning loop](#the-learning-loop)** · **[wiki](#the-wiki)** · **[the stranded lane](#the-lane-nobody-could-reach)** · **[vs claude-mem](#vs-claude-mem)** · **[the number](#the-number)** · **[cli](#cli)** · **[faq](#faq)**

</div>

---

> [!IMPORTANT]
> Claude Code already writes a transcript of every session to `~/.claude/projects/`. Most memory plugins ignore that file and rebuild capture from scratch: lifecycle hooks feeding a background worker, a vector database, summarization calls billed to your token budget — an elaborate machine for forgetting most of what happened. This tool skips capture and indexes what is already on disk. All of it.

The first thing this tool found on my machine was a conversation I'd forgotten, where Claude and I had already evaluated a memory plugin two weeks earlier and reached the same conclusion. That sold me.

## features

**Search everything, instantly.** Every message of every session — including the tool calls and their output, and including the subagents you fanned out — BM25 over FTS5, in milliseconds. `--semantic` adds local vectors for meaning-only queries, so asking about "trackpad dragging" finds the touchpad rows.

**Retrieval you don't have to remember.** Every prompt queries the index on `UserPromptSubmit`. Your own history arrives as context before Claude answers. There is no command to forget to run.

**A published retrieval number.** `cml eval` measures recall@k against your real history, no labelling required. As far as I can tell it is the only such figure any Claude Code memory plugin publishes.

Around those three:

- a **learning loop** that collects per-turn signals and folds them into memory Claude actually loads
- a **wiki** of markdown pages, one topic each, Obsidian opens the folder as a vault
- **chronic loops** (`cml loops`), the asks that keep coming back unresolved
- **prompt hints** that flag a message as a correction, preference, or decision worth keeping
- a **durability gate**, so the map holds a few hundred hard-won facts instead of every true sentence

Nothing runs in the background. The binary executes on a hook and exits in milliseconds; RAM at rest is zero. One SQLite file, on your machine, that never leaves it.

## install

```
/plugin marketplace add MiracleWeb3/claude-memory-light
/plugin install claude-memory-light
```

The plugin fetches a prebuilt binary on first run, or builds from source with cargo (needs a Rust toolchain and the sqlite3 headers; `curl` is only needed at runtime, and only if you turn on distillation). Then:

```bash
cml index --all   # first full index: 50 sessions ≈ 2 s
cml doctor        # sanity check
```

> [!WARNING]
> Claude Code deletes transcripts after about 30 days by default. Set `"cleanupPeriodDays": 3650` in `~/.claude/settings.json` or your memory has an expiry date.

## use

```bash
cml search "wireguard cyprus"                # across ALL sessions, memory notes, wiki
cml search parser --project myapp --limit 20
cml search deploy --role wiki                # only curated wiki pages
```

<div align="center">
<img src="assets/demo.svg" width="880" alt="cml search demo"/>
</div>

Three bundled skills teach Claude to search memory before re-solving old problems, to consolidate learning signals when they pile up, and to keep the wiki current. You don't run anything by hand.

> [!TIP]
> No hits doesn't mean not found. Try a second and third keyword set: synonyms, error text, filenames. The skill teaches Claude to do exactly that before giving up.

## how it works

Three stores, and a hook on both sides of each one. Nothing here waits to be asked.

<div align="center">
<img src="assets/architecture.svg" width="880" alt="Every store cml writes, the hook that writes it, and the hook that reads it back"/>
</div>

<details>
<summary><b>📁 where everything lives</b></summary>

```
~/.claude/claude-memory-light/
├── index.db          # the FTS5 index (disposable, rebuilds in seconds)
├── inbox/            # learning-loop signals, one file per project
│   └── myapp.md
├── spill/            # tool output too big for the context window, kept verbatim
│   └── <session>/
├── wiki/             # your wiki pages
│   └── topic.md
└── bin/cml           # the binary (installed by the bootstrap)
```

Transcripts stay where Claude Code puts them. `cml` never moves or modifies them.

</details>

## recall — the read half

Capture was hooked from the first commit. Retrieval never was, and that asymmetry is the whole story.

Measured across 564 transcripts on the machine this was built on:

```
write side (hooked)      625 runs
read side  (not hooked)   20 runs, in 12 sessions — 2%
```

The index was in good shape and almost nobody read it. Reading it was a decision the model had to remember to make, and a decision made 2% of the time is indistinguishable from a feature that does not exist.

So `cml recall` runs on `UserPromptSubmit` and the model is not consulted. Every prompt queries the index; whatever clears the gates arrives as context before Claude answers.

The gates are what keep it from becoming wallpaper, and they were tuned against the corpus rather than guessed:

- **rarity** — a term appearing in more than 6% of the index is dropped before the query runs. Without it, *"do we have the same problem in all the code"* matched on `code` and `problem` and dragged in a row from an unrelated project. Firing rate before this gate existed: **76%** of real prompts. After: **28%**.
- **substance** — fewer than two surviving terms means the prompt asks nothing. `yes`, `ok`, `do it` recall nothing.
- **overlap** — a row sharing one word with your prompt is a coincidence. Two is a topic.
- **echo** — a row that *is* your prompt, asked once before, tells Claude nothing that isn't already on screen. The answer to it might; the echo never does.
- **novelty** — a row already injected this session is not injected again.

Fires on about half the prompts it sees, mean **2.1** hits, **~550 bytes** injected, and the whole hook runs in **milliseconds**. One FTS5 query — the embedding leg was measured to make this path worse and was removed from it (see [the number](#the-number)). Still no LLM call anywhere in it.

## the learning loop

A Stop hook appends your message from each turn to a per-project inbox file, flagged when it reads like a correction. At session start, once five or more signals accumulate, the briefing carries them.

It used to carry a count and a file path instead, with an instruction to go and read them. That reminder fired every session and was correct every time, and the signals still sat there: on the machine this was built on, a dozen of them had survived ninety sessions of being accurately reported. Nothing was broken. Noticing a line and moving on is free, so that is what kept happening. Anything that depends on somebody choosing to invoke it is, in practice, a thing that does not run — which is the same lesson `cml recall` had already learned at 2%, arrived at a second time from the other direction.

So the signals themselves arrive now, grouped with corrections first, capped at 12 of them so the briefing cannot become wallpaper. `cml consolidate` prints the same report on demand, and `--clear` retires the lines it just printed. Neither writes a memory file. Unreviewed facts written into memory by a machine is the failure mode this whole category is prone to, and being pushy about *surfacing* does not require being careless about *writing*. The hooks contain no LLM calls; the distillation happens inside a session you were going to run anyway, where the full context already lives.

That same SessionStart hook also briefs you on what's still open: chronic asks that keep recurring across sessions (the logic behind `cml loops`, capped to the top 3), and a menu of wiki topics on file so Claude knows what it can pull in before re-deriving something already written down. Both are skipped on `resume`/`compact` sources — re-injecting static context on every resume is exactly the bloat this is budgeted against — but the inbox nag above still fires there if it's due. The whole message reports its own size inline (`[context injected: N.NkB]`): measured 1.2kB on a fresh start on this repo's own index, 0.5kB on resume where only the nag can still trigger.

A third hook, sharing `UserPromptSubmit` with recall above, classifies each prompt against a phrase table — correction, preference, decision, method, reference — and once per session per category, injects a one-line nudge to capture it (`cml hint`), e.g. *"reads like a durable preference — consider capturing it so future sessions inherit it."* It's a suggestion, never a write: the model still decides what's worth keeping. Still zero LLM calls anywhere in the loop, just SQL and string matching.

## the wiki

A folder of markdown files, one page per topic, edited in place when facts change. Old states aren't lost; the transcripts keep them. Obsidian opens the folder as a vault. `cml search <topic> --role wiki` finds pages, and the bundled skill keeps Claude writing them.

## the lane nobody could reach

For most of this project's life it indexed 57,683 rows of tool output that `cml search` could not return. Not a broken query: the SQL for that lane was correct and sitting in the search file. Nothing on the command line ever selected it. `--role work` answered **no hits** against a corpus holding 57 matches for the same query, because the flag parser and the ranker had been written at different times and never agreed on what a lane was.

One caller could see those rows, the SessionStart hook, so the feature looked alive from the inside. The person who owned the data could not reach it from a terminal.

Two other holes came out with it. The indexer walked top-level transcripts only, skipping 784 of 1,008 files, which is every subagent — so all the parallel fan-out work was absent from memory. And the Stop hook that collects learning signals had a project filter left over from when the feature was being trialled on one repo, so every other project on the machine recorded nothing.

The fix that mattered was structural rather than a patch. One `Lane` type now feeds both the `--role` parser and the ranker, and both match on it exhaustively, so a lane that exists without a way to reach it fails to compile. `every_lane_is_reachable_from_the_cli` is the test that says so. With no `--role` at all, search ranks every lane together and merges them, because you should not have to know which table holds your answer. `cml doctor` prints the per-lane counts and checks reachability while it runs:

```
lanes           : conversation 5234 · tools 123374 · scene 72 — all reachable from --role
```

Nothing was lost while this was broken; the rows were on disk the whole time. That is the part worth keeping in mind about any memory tool, this one included. Storage is easy to verify and easy to feel good about. Whether you can get anything back out is a separate question, and it wants a separate test.

## when it breaks

There is no worker process to die. `capture`, `nudge`, `hint`, and `recall` exit 0 on every code path, including total failure, so a broken install degrades to "no memory" instead of "no Claude". When the binary itself is missing, the bootstrap still emits valid passthrough JSON for all four.

> [!NOTE]
> The index is disposable. Transcripts are the source of truth, and everything rebuilds from them in seconds:
> ```bash
> rm ~/.claude/claude-memory-light/index.db*
> cml index --all
> ```

## vs claude-mem

claude-mem is the popular one, and it works for plenty of people. It also runs a persistent Bun worker on a local HTTP port (default `37700 + uid % 100`, historically 37777), needs Node plus Bun plus a Python vector database, and summarizes your session with LLM calls while you work. Users on Pro plans have burned a [full 5-hour token budget in under 10 messages](https://github.com/thedotmack/claude-mem/issues/618) with it enabled. When the worker doesn't come up, its hook has [failed in a loop and blocked prompts](https://github.com/thedotmack/claude-mem/issues/2926). I read that issue tracker for an afternoon and wrote this instead.

| | claude-memory-light | claude-mem |
|---|:---:|:---:|
| background processes | ✅ none | ❌ Express worker, port 37777 |
| calls against your Claude plan | ✅ never, on any code path | ❌ summarization runs on it |
| other LLM calls | ⚠️ none by default; optional curation calls a model you configure | ❌ required, built in |
| extra runtimes | ✅ none | ❌ Bun + Node + Python/uv + Chroma |
| RAM at rest | ✅ 0 | ❌ 50 MB and up, leak reports exist |
| hook failure mode | ✅ exit 0, session unaffected | ❌ can block all prompts |
| works on subscription plans | ✅ that's the point | ⚠️ [budget burned in under 10 messages](https://github.com/thedotmack/claude-mem/issues/618) |
| search | ✅ BM25 + doc2query expansions; vectors on `--semantic` | ✅ FTS5 + vector (Chroma) |
| automatic retrieval | ✅ every prompt, gated, **measured** | ✅ at session start, progressive disclosure |
| what's kept | ✅ every message, verbatim, forever | ⚠️ an LLM summary; the rest is gone |
| sessions from before you installed it | ✅ all of them — the transcripts were already there | ❌ none |
| a search hit is | ✅ the actual message | ⚠️ a paraphrase of it |
| retrieval quality | ✅ **published, with the command to reproduce it** | ❓ no number published |

### the number

Every comparison table on the internet, including the one above, is adjectives. So this ships the benchmark instead:

```bash
cml eval          # recall@k over YOUR history
```

No labelling needed — when you asked something at turn N, the assistant answered at turn N+1, so that row *is* the ground truth. Replay the question, see whether the answer comes back. On the corpus this was developed against (1,171 rows, 272 question/answer pairs):

```
fired           : 136 (50%)
recall@3        : 40 (14.8% of all questions, 29.4% of the ones it answered)
recall@1        : 30 (11.1%)
```

That is not a great number. It is a **real** one, it is reproducible on your own machine, and it is the only such number published by any Claude Code memory plugin — so treat any competitor's "semantic understanding" claim, including the one this README used to make, as unmeasured until someone prints a figure next to it.

### what the benchmark cost us

It immediately killed a feature this project had been advertising. `cml eval --vectors` re-runs with the embedding leg on:

| retrieval | recall@3 | recall@1 |
|---|:---:|:---:|
| BM25 + doc2query | **40 (14.8%)** | **30 (11.1%)** |
| \+ embedding rerank | 35 (13.0%) | 21 (7.8%) |

The vector leg was making retrieval **worse** — and the previous version of this section claimed "the vector gap is closed" as a selling point. Four measurements across two corpus states said otherwise, so it was removed from the automatic path. We then wrote a BERT encoder from scratch (bge-small-en-v1.5 — the model our static one was distilled *from*) to check whether a real contextual model would win. It halved the damage and still lost to plain BM25.

The cause is structural: retrieval requires two shared content words before a row is injected, so a purely semantic match cannot survive the pipeline however the fusion is arranged. Vectors stay for `cml search --semantic`, where they do something BM25 genuinely cannot — asked for *"trackpad dragging"* it returns **touchpad** rows.

**Where claude-mem is ahead:** it compresses. Fifty LLM calls buy a summary of a 200k-token session; we have no equivalent and don't attempt one, because we retrieve the original instead. It is also vastly more adopted, and adoption is not something a benchmark fixes.

**vault-template plugins** (e.g. [obsidian-mind](https://github.com/breferrari/obsidian-mind)) are a folder of markdown plus an instruction manual telling Claude how to file notes into it. Read the code before the stars.

Of obsidian-mind's 27 commands and agents, three do memory. The rest is performance-review tooling: brag docs, 1:1 trackers, standup generators. The "brain" ships as empty placeholder files.

Nothing is captured unless you run a command, so it remembers exactly what you remembered to tell it. That is a diary with extra steps, not memory. Semantic search is outsourced to an optional external engine wanting ~1.6 GB of local models and ~1.28 GB of RAM per reranked query; when it isn't installed, "semantic search" quietly means grep. The filing instructions load into every session, thousands of tokens deep, before you type a word.

Your memory already exists. It's the transcripts. Index them, and don't make a human the capture hook.

| | claude-memory-light | vault-template plugins |
|---|:---:|:---:|
| capture | ✅ automatic, every transcript already on disk | ⚠️ manual, only what gets filed via a command |
| semantic search | ✅ hybrid FTS5 + vectors, in the one binary | ⚠️ typically a separate tool, GB-scale local model |
| standing context cost | ✅ ~0 standing; briefing measured at 1.2kB on a fresh start, 0.5kB on resume | ⚠️ always-loaded filing instructions, thousands of tokens/session |

## cli

| command | what it does |
|---|---|
| `cml index [--all]` | incremental (or full) reindex of transcripts, memory notes, wiki |
| `cml search <terms> [--project P] [--role R] [--limit N] [--semantic\|--keyword]` | hybrid ranked search |
| `cml embed [--all]` | build (or rebuild) the semantic index — one-time init, then automatic |
| `cml forget <rowid...>` \| `--match "<q>" [--yes]` | purge junk memories, blocklisted so reindexing never resurrects them (`--clear` undoes) |
| `cml distill [--all] [--limit N]` | optional LLM curation, see below |
| `cml loops [--days N] [--limit K]` | chronic-loop detection: asks recurring across ≥2 sessions in the window, most-recurrent first (default 30 days, top 10) |
| `cml consolidate [--all] [--clear]` | group pending learning signals into a reviewable report; `--clear` retires only the lines it just reported, and writes no memory files |
| `cml state [--project P] [--budget N]` | the standing brief for a project: what is still open, what recently got done |
| `cml stats` | row counts, knowledge count, DB size |
| `cml doctor` | environment check, graphify detection, which binary is running, and what the last background curation run actually did |
| `cml version` | version of *this* binary — the hooks run an installed copy, so it need not match the repo you are reading |
| `cml capture` | *(hook)* append turn's user message to the learning inbox |
| `cml nudge` | *(hook)* SessionStart briefing: learning-inbox nag (always eligible), plus open loops and wiki topics (skipped on resume/compact) |
| `cml hint` | *(hook)* UserPromptSubmit: phrase-table classifier nudges a capture, once per session per category |
| `cml recall` | *(hook)* UserPromptSubmit: retrieves against the prompt and injects the top matches, gated on rarity, substance, overlap, echo and novelty |
| `cml offload` | *(hook)* PostToolUse: spills oversized tool output to `spill/<session>/` and leaves a one-line marker in its place |
| `cml eval [-k N] [--limit N] [--vectors] [--no-asks]` | recall@k against your own history; the flags ablate the embedding leg and the doc2query expansions so any claim here stays checkable |

<kbd>CML_HOME</kbd> moves the data directory (default `~/.claude/claude-memory-light`). <kbd>CML_NUDGE_THRESHOLD</kbd> tunes the nudge, default 5. <kbd>CML_EMBED_MODEL</kbd> swaps the embedding model: `minishlab/potion-base-32M` for better recall, `minishlab/potion-multilingual-128M` for non-English corpora. Run `cml embed --all` after switching.

### curation, if you want it

`cml distill` is optional. A cheap external model (DeepSeek by default) judges each row on two independent questions: is there content here, and is it worth a permanent point on the map? A row that fails the second still stays searchable, it just carries no gist.

Once a key sits in `llm.key`, `cml index` starts this **detached in the background**. It costs about 20 seconds a row, so it can never run on the Stop hook's clock. Whatever the last run did shows up in `cml doctor`.

<details>
<summary><b>bring your own curator</b></summary>
<br/>

The distillation layer speaks to any OpenAI-compatible `/chat/completions` endpoint. Drop an API key into `~/.claude/claude-memory-light/llm.key` and it activates; two env vars point it anywhere:

```bash
# DeepSeek (default — nothing to configure but the key)
CML_LLM_URL=https://api.deepseek.com/chat/completions   CML_LLM_MODEL=deepseek-v4-pro

# OpenRouter — any model on the router
CML_LLM_URL=https://openrouter.ai/api/v1/chat/completions   CML_LLM_MODEL=deepseek/deepseek-chat

# GLM / Zhipu
CML_LLM_URL=https://open.bigmodel.cn/api/paas/v4/chat/completions   CML_LLM_MODEL=glm-4-flash
```

No key, no calls — the curator is off by default and everything stays on your machine.

</details>

## faq

<details>
<summary><b>does my data leave the machine?</b></summary>
<br/>

No. One SQLite file in your home directory. No cloud, no sync, no telemetry, no accounts.

</details>

<details>
<summary><b>does it cost tokens?</b></summary>
<br/>

**None of your Claude budget, on any code path.** That's the reason this exists.

Indexing, search, recall and embedding are pure local compute — SQLite plus a local embedding model. No network, no model call.

One feature is the exception and it is off until you turn it on: **curation** (`cml distill`). If you put a key in `~/.claude/claude-memory-light/llm.key`, the Stop hook will judge new rows through whatever OpenAI-compatible endpoint you configured — DeepSeek by default, capped at 40 rows per rubric per turn. That is a cheap external model you chose and pay for separately; it never touches your Claude plan. Delete the key file and it stops. With no key there is no network call anywhere in this tool.

</details>

<details>
<summary><b>how does semantic search work without an API?</b></summary>
<br/>

A Model2Vec static embedding model (~30 MB) runs locally — it's a lookup table plus mean pooling, so embedding is effectively instant even on weak hardware. Vectors sit in a plain table inside the same index.db, little-endian f32, and similarity is a brute-force cosine sweep across them in parallel. At this corpus size that is 5.7 MB of floats and the sweep finishes faster than the query parse in front of it, which is why there's no vector extension and no index to keep warm. `cml embed` builds it once (needs network for the one-time model download); after that everything is offline. Queries run both legs — BM25 and cosine — and fuse the rankings by position, since BM25 scores and cosine similarities are not on a comparable scale. `--keyword` or `--semantic` forces a single leg.

</details>

<details>
<summary><b>why Rust?</b></summary>
<br/>

It went Rust, then C++, then back. The C++ round was argued on directness: sqlite3 is a C library either way, and calling it without a binding crate in between is one less layer. That held up. What it cost was everything the compiler stops checking for you, in a binary wired into a hook that runs on every prompt of every session.

The return trip paid for itself in deletions rather than in speed. `sqlite-vec` was 8,315 lines of vendored C carrying a similarity search over 5,576 × 256 floats; that is a rayon cosine sweep now, and it was also the only reason the binary had to load an extension, so `unsafe_code = "forbid"` holds across the whole crate. The hand-rolled SQLite wrapper is rusqlite. The hand-rolled JSON reader is serde_json. The UTF-8 scanning is gone because `str` is UTF-8 by construction.

Counting both trees the same way, production code went 5,649 → 4,230 lines, tests went 1,132 → 1,747, and the vendored C went to zero: 15,096 lines down to 5,977, across 81 files down to 36. 24 crates, which is worth watching in a language where `cargo add` costs nothing at the moment you type it.

</details>

<details>
<summary><b>windows?</b></summary>
<br/>

Untested. The transcript format is the same and nothing here is platform-specific, so it should be close. PRs welcome.

</details>

<details>
<summary><b>how big does the index get?</b></summary>
<br/>

About 11 MB for 50 sessions / 4,000 messages on my machine. SQLite FTS5 handles orders of magnitude more without noticing.

</details>

## roadmap

- [x] FTS5 index over transcripts, memory notes, wiki
- [x] learning loop (capture + nudge)
- [x] plugin packaging, prebuilt binaries
- [x] semantic search — local Model2Vec embeddings, hybrid RRF, same file, no daemon
- [x] C++ → Rust port — `unsafe_code = "forbid"`, the vendored C dropped, 60% less code
- [x] session briefing — chronic open loops and a wiki topic menu folded into the SessionStart nudge, capped and size-metered
- [x] `cml loops` — chronic-loop detection: asks that recur across sessions, surfaced from the index
- [x] `cml hint` — UserPromptSubmit phrase-table classifier that nudges a capture, once per session per category
- [x] `cml recall` — the read half hooked: every prompt retrieves against the index, so recall stops depending on the model remembering to search
- [x] `cml eval` — recall@k over your own transcripts, no labelling; the first published retrieval number for a Claude Code memory plugin
- [x] doc2query — the curator writes how you would *search* for a row, indexed beside it
- [x] a BERT encoder written from scratch — built to test whether contextual vectors beat BM25 here. They do not. Kept for `--semantic`, dropped from the automatic path.
- [x] subagent transcripts indexed — 784 of 1,008 files used to be skipped, so every parallel fan-out was missing from memory
- [x] one `Lane` type across the `--role` parser and the ranker, so an unreachable lane fails to compile instead of quietly answering "no hits"
- [x] `cml consolidate` — the pending learning signals arrive inside the SessionStart briefing instead of a note telling you to go read them
- [ ] optional end-of-session digests (batched, single call, opt-in)
- [ ] windows support

## star history

<div align="center">

[![Star History Chart](https://api.star-history.com/svg?repos=miracleweb3%2Fclaude-memory-light&type=Date)](https://star-history.com/#MiracleWeb3/claude-memory-light&Date)

</div>

## license

[MIT](LICENSE) © [MiracleWeb3](https://github.com/MiracleWeb3)

<div align="center">
<img src="https://capsule-render.vercel.app/api?type=waving&height=120&color=gradient&customColorList=12&section=footer" width="100%" alt=""/>

**[⬆ back to top](#top)**

</div>
