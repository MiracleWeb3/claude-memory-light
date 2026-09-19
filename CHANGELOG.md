# changelog

## 3.1.0 — every harness, and memory you can share

**Every harness, one index.** `cml index` no longer reads only
`~/.claude/projects`. Adapters normalise jcode (a JSON document per session),
opencode (`session`→`message`→`part` in one SQLite file, opened read-only so it
cannot lock the editor) and Codex (rollout JSONL) into the same two lanes, with
the same signal floors — imported from the Claude path rather than retyped, so
the lanes cannot drift into judging the same text differently.

Measured on the machine this was built on: 1,523 jcode sessions → 80,357 rows,
searched in 231 ms.

**Attribution without a migration.** Row origin lives in a sidecar `origin`
table, not a column: `mem` and `work` are FTS5 and do not take `ALTER TABLE ADD
COLUMN`. It is keyed by file, because FTS5 reissues rowids after a delete and a
rowid-keyed origin would eventually credit a stranger's row to you. Absent means
what it always meant — your own Claude transcript — so an existing index needs
no migration at all.

**Sharing.** `cml share` exports a session or project as a single-file
`.cmlpack`; `cml import` makes it searchable and marks every line `[peer]`;
`cml forget --from <peer>` removes exactly those rows. Import is idempotent.

**Redaction, on by default.** Vendor API keys, PEM private-key blocks, JWTs,
`Authorization` headers, assigned secrets and other people's home paths are
stripped from the exported copy — never from the local index. `--dry-run`
reports a count per class. A false-positive corpus (git hashes, UUIDs, minified
JS, base64 images, `PATH=`) is part of the suite.

Two real leaks were found by the end-to-end run and fixed:

- the dedup key embeds the row's first 64 characters verbatim, so a key minted
  before the scrub carried the secret into a column nobody inspects;
- the assignment detector read the rest of the line as the value, so a secret
  quoted inside prose contained a space, failed the opacity check, and passed
  through untouched.

**Installation.** `cml install` detects every harness and patches each config in
its own format; `cml uninstall` reverses it exactly. Configs are parsed and
rewritten by identity, never by regex, and backed up first. `cml harnesses`
prints the support matrix including the column where the answer is no.

**MCP.** `cml mcp` serves `memory_search` over stdio JSON-RPC for jcode and
opencode, which have no prompt-injection hook and therefore cannot have memory
arrive on their own.

**Fixed:** a closed stdout is exit 141 rather than a panic, so `cml search x |
head -3` no longer prints a backtrace.
