# claude-memory-light

Full session memory for Claude Code. One small Rust binary. No daemons, no LLM calls.

Claude Code already writes a transcript of every session to `~/.claude/projects/`. Most memory plugins ignore that file and rebuild capture from scratch: lifecycle hooks feeding a background worker, a vector database, summarization calls billed to your token budget. I didn't want to pay tokens to remember my own conversations, so this tool skips capture and indexes what is already on disk.

`cml` puts every transcript, your memory notes, and a personal wiki into one SQLite FTS5 database. A search returns in milliseconds and costs nothing. A Stop hook keeps the index fresh; the incremental pass takes about 17 ms on my 4-core laptop. First full index of 50 sessions took 2 seconds.

## install

```
/plugin marketplace add MiracleWeb3/claude-memory-light
/plugin install claude-memory-light
```

The plugin fetches a prebuilt binary on first run, or builds with cargo if you have Rust. Then:

```bash
cml index --all
cml doctor
```

One setting worth changing: Claude Code deletes transcripts after about 30 days by default. Set `"cleanupPeriodDays": 3650` in `~/.claude/settings.json` or your memory has an expiry date.

## use

```bash
cml search "wireguard cyprus"
cml search parser --project myapp --limit 20
cml search deploy --role wiki
```

```
2026-07-08 assistant home     29c3e5e3 | … Brave still sends all [rezka] traffic to the old, now-dead address …
2026-07-05 user      parserx  cda63721 | … my honest verdict: [claude-mem] is a good tool, but for us …
```

The second hit up there is real. The first thing this tool found on my machine was a conversation I'd forgotten, where Claude and I had already evaluated a memory plugin two weeks earlier and reached the same conclusion. That sold me.

Three bundled skills teach Claude to search memory before re-solving old problems, to consolidate learning signals when they pile up, and to keep the wiki current. You don't run anything by hand.

## the learning loop

A Stop hook appends your message from each turn to a per-project inbox file, flagged when it reads like a correction. At session start, once five or more signals accumulate, Claude gets a note telling it to distill them into its persistent memory and clear the inbox. The hooks contain no LLM calls. The distillation happens inside a session you were going to run anyway, where the full context already lives.

## the wiki

A folder of markdown files at `~/.claude/claude-memory-light/wiki/`, one page per topic, edited in place when facts change. Old states aren't lost; the transcripts keep them. Obsidian opens the folder as a vault. `cml search <topic> --role wiki` finds pages, and the bundled skill keeps Claude writing them.

## when it breaks

There is no worker process to die. `capture` and `nudge` exit 0 on every code path, including total failure, so a broken install degrades to "no memory" instead of "no Claude". The index is disposable:

```bash
rm ~/.claude/claude-memory-light/index.db*
cml index --all
```

Transcripts are the source of truth. Everything else rebuilds from them in seconds.

## why not claude-mem

claude-mem is the popular one, and it works for plenty of people. It also runs a persistent Express worker on port 37777, needs Bun plus a Python vector database, and summarizes your session with LLM calls while you work. Users on Pro plans have burned a [full 5-hour token budget in under 10 messages](https://github.com/thedotmack/claude-mem/issues/618) with it enabled. When its worker dies, hooks have [locked up every prompt in the session](https://github.com/thedotmack/claude-mem/issues?q=worker+unreachable). I read that issue tracker for an afternoon and wrote this instead.

|  | claude-memory-light | claude-mem |
|---|---|---|
| background processes | 0 | Express worker, port 37777 |
| LLM calls per session | 0 | ~50 summarization calls |
| runtimes | none | Bun, Node, Python/uv, Chroma |
| RAM at rest | 0 | 50 MB and up; leak reports exist |
| hook failure | exit 0, session unaffected | can block all prompts |
| search | FTS5, keyword | FTS5 + vector |

Vector search is the one real feature gap. FTS5 with Porter stemming has covered everything I've asked of it so far. If it stops being enough, sqlite-vec drops into the same database file without a daemon, and that's the planned path.

## cli

| command | what it does |
|---|---|
| `cml index [--all]` | incremental (or full) reindex of transcripts, memory notes, wiki |
| `cml search <terms> [--project P] [--role R] [--limit N]` | ranked search |
| `cml stats` | row counts, DB size |
| `cml doctor` | environment check |
| `cml capture` | hook: append turn's user message to the learning inbox |
| `cml nudge` | hook: consolidation reminder once signals pile up |

`CML_HOME` moves the data directory (default `~/.claude/claude-memory-light`). `CML_NUDGE_THRESHOLD` tunes the nudge, default 5.

Built with rusqlite (bundled SQLite) and serde_json. Nothing else.

## license

[MIT](LICENSE)
