# The tool harness

Shortlist sends every chat question to the model, but gives it a compact tool
catalogue and capped, local results instead of resume excerpts. The tools do the
cheap retrieval; the model reads the evidence and writes the answer. This
document explains the design, the wire format, and how to extend it.

The crate is `crates/harness` (`shortlist-harness`).

## Why

The original chat path pasted four retrieved excerpts into one prompt. That has
three problems:

1. **It scales with the excerpt, not the question.** Four 700-character
   summaries are ~700 tokens before the model sees anything.
2. **It answers less than it costs.** The retriever returns whole summaries even
   when the question only needs a skill, a year count, or one sentence of
   evidence.
3. **Multi-turn context grows behind the app's back.** Each prompt and answer
   stays in the OpenCode session, so turn ten pays for turns one through nine.

The harness inverts all three:

- Tools run locally over SQLite, the FTS index, and the local vectors.
- Results are compact rows with short handles, not prose.
- Every result has a hard token cap and every turn has a hard budget.
- Each model call starts from a **fresh session** containing only the current
  compact context.
- A local prefetch runs the obvious tool first, so the model usually answers in
  one bounded round instead of choosing tools blind. The model still writes the
  final answer; local direct rendering is opt-in (`Config::direct`) for offline
  tooling and benchmarks only.

## Flow

```text
question
   │
   ├─ local intent router
   │    ├─ one candidate named → get (+ read when it asks for detail)
   │    ├─ two+ candidates named → compare
   │    ├─ aggregate question → stats
   │    └─ otherwise → shortlist (3–10 rows) or find
   │
   ├─ tool result (capped, short handles)
   │
   └─ model call ──▶ answer
        ├─ prefetched result
        ├─ top 3 profiles with roles
        └─ may request more tools (≤1 extra round)
```

Every answer is model-written. The prefetch exists to make the prompt cheap, not
to bypass the model. It turns "tell me about Sarah" into one compact profile
instead of four resume excerpts; the model then states the answer in its own
words. Evaluative questions — best, top, strongest, compare, recommend, should I
hire, why, summarise, assess — get the same treatment plus a researched shortlist.
The model is never asked to repeat a local pre-score as its verdict.

## Tools

| Tool | Call | Returns |
|:--|:--|:--|
| `stats` | `stats [n]` | Candidate count, experience range, top skills, locations, file types |
| `find` | `find q [skills any miny loc n p sort]` | `id\|name\|title\|yrs\|loc\|skills` |
| `shortlist` | `shortlist q [skills any miny loc n]` | `id\|name\|pre\|req\|any\|yrs\|loc\|skills\|why` |
| `get` | `get id [f chars]` | One compact profile, including structured roles, education, and certifications |
| `read` | `read id q [w n]` | Matching windows from one resume |
| `grep` | `grep q [ids w n]` | Matching snippets across candidates |
| `note` | `note id` | The recruiter's note (read-only for model calls) |
| `compare` | `compare ids [f len]` | One aligned row per candidate |

`shortlist` is the workhorse. It fuses BM25 and vectors, applies required
skills, nice-to-haves, minimum years, and location **in Rust**, then adds role
signals: seniority from titles, recency of the latest role, and whether the
query terms appear in the role descriptions. It returns a 0–100 **pre-score**
with the matched required/nice-to-have counts and a quoted evidence snippet
from the current role. `pre` is retrieval evidence, not the model's verdict.
`find` stays lexical and exact; `shortlist` is the semantic ranker.

A ten-candidate shortlist costs roughly 1 KB — about 250 tokens — versus tens
of kilobytes for their full profiles. Above five rows the evidence column is
dropped; ask `get` or `read` for detail on the winners.

`note` is read-only for model calls. The tool can write, but a model run is
refused, so a resume cannot talk the model into changing a recruiter's note.
Note edits stay in the candidate drawer.

Contact questions are a special case: `email` and `phone` are not part of the
default compact profile, so the router opts them in when the question asks for
them. A contact-only lookup prefetches just the requested field. In opt-in
direct mode it returns that field and states when the library has no value; in
chat the model receives the same focused result and answers.

## Wire format

JSON function calling repeats schema keys on every call. The harness uses one
line per call:

````text
```shortlist
shortlist q="rust fintech" skills=Rust miny=5 n=8
get id=a1 f=summary,skills
```
````

Rules:

- One call per line: `tool key=value key=value`.
- Values with spaces are quoted; values without quotes run to the next space.
- The fence tag is `shortlist`; `tool` and `tools` are accepted too.
- `@tool ...` lines are accepted as a fallback.
- A reply made entirely of call lines is treated as a call block.
- Anything else is the final answer.

The model never sees candidate UUIDs. The run assigns handles (`a1`, `a2`, …)
and resolves them before touching the store. Answers cite `[a1]`; the harness
expands citations to names before the answer is stored, so the UI still links
them to candidate chips.

## Token discipline

| Guard | Default |
|:--|:--|
| Per tool result | 360 tokens |
| Per turn, all tool results | 1,600 tokens |
| Recruiter note in a profile | 240 chars |
| Resume window (`read`/`grep`) | 40–400 chars per window |
| Answer | 12,000 chars |
| Conversation history | last 6 messages, 240 chars each |
| Model tool rounds | 1 extra round |

The estimator is `chars / 4` for ASCII and `chars` for non-ASCII, rounded up. It
is a budgeting guard rail, not a tokenizer; expect roughly ±15%.

Other savings:

- **Handles** replace 36-character UUIDs with 2–4 characters.
- **Duplicate calls** are answered with `already shown` from a per-run cache.
- **Empty fields** are omitted from profiles; defaults are implicit.
- **Column order** is declared once in a `cols:` line instead of repeating keys.
- **Skill lists** are comma-joined and capped with `+N more`.
- **Evidence** is clipped on a word boundary and omitted above five rows.
- **`stats`** answers aggregate questions from metadata without any resume text.
- **Research pack** gives the model the top three profiles before it ranks, so it
  judges experience instead of trusting the local `pre` order.

### What "99% less" means

Every tool reports `raw_chars`: the size of the naive payload it replaced. For
`find` and `shortlist` that is the whole library's resume text; for `get`,
`read`, and `grep` it is that candidate's resume. The run report compares
`raw_chars` with the compact bytes actually emitted. It does **not** claim to
measure a provider bill.

Measured on the twelve-candidate demo library:

| Call | Compact | Raw | Reduction |
|:--|--:|--:|--:|
| `find q=rust n=5` | 358 chars | 4,794 chars | 92.5% |
| `shortlist q="rust" n=5` | 794 chars | 4,794 chars | 83.4% |
| `stats` | 329 chars | 4,794 chars | 93.1% |

For chat, add one model call on top of one of these compact results. The model
reads the tool output and writes the answer; it never receives the raw resume
text. A lookup is one bounded prompt of a few hundred tokens. An evaluative
question is one bounded prompt plus the research pack.

For evaluative questions the harness pays for judgement **once**: a compact
shortlist, three researched profiles, and room for one batch of follow-up
tools. It never grows a session, so five follow-ups cost five bounded prompts,
not five increasingly large ones.

## Native MCP tools

The harness is also a local MCP server (`crates/mcp`, binary `shortlist-mcp`).
That is the preferred path for OpenCode: the model gets real JSON schemas and
calls tools natively; the text protocol remains a fallback.

```bash
cargo build -p shortlist-mcp
shortlist-cli mcp install
shortlist-cli mcp status
```

Or press **Register MCP** in **Settings**. The install command writes a
global OpenCode MCP entry named `shortlist`, pointing at the helper binary and
the current `SHORTLIST_DATA_DIR`, and disables Code Mode for this server so the
eight tools appear as direct native tool definitions. OpenCode reloads MCP
configuration without a restart.

MCP tool names differ only where the word would repeat:

| MCP tool | Harness tool |
|:--|:--|
| `stats` | `stats` |
| `find` | `find` |
| `screen` | `shortlist` |
| `get`, `read`, `grep`, `note`, `compare` | same |

The server keeps the handle table across calls, so `screen` → `get a1` works.
`get`, `read`, and `compare` also accept a full candidate name, which is the
most reliable argument when the model mixes native and text-protocol tools.
The MCP server is read-only for notes.

The server instructions added to OpenCode's system prompt say the same thing as
the text harness: `pre` is a retrieval pre-score, inspect the top profiles, and
explain gaps before giving a verdict.

## Inspecting it

No model is required to exercise the tools:

```bash
shortlist-cli tools
shortlist-cli tool find q=rust n=5
shortlist-cli tool shortlist q="payments infrastructure" skills=Rust miny=5
shortlist-cli tool get id=demo-01
shortlist-cli db chunks
shortlist-cli reindex
shortlist-cli tokens "Who has the strongest Rust background?"
shortlist-cli ask "Who has the strongest Rust background?"
shortlist-cli ask --plain "Summarise the backend profiles"
```

`tokens` compares the old excerpt prompt with the new bounded prompt. Every chat
question calls the model; the report shows the compact tool payload plus the
model prompt that will carry it. `db chunks` prints the real UUIDs, chunk text,
and an embedding preview (`dim`, `L2`, first values). `ask` prints the tool-call
count, rounds, prompt/reply estimates, and the raw-vs-compact reduction.

Set `SHORTLIST_DATA_DIR` to keep a scratch library away from your real one.

## Extending

1. Implement `Tool` in `crates/harness/src/tools/`:

```rust
impl Tool for MyTool {
    fn name(&self) -> &'static str { "mine" }
    fn signature(&self) -> &'static str { "mine id [n] → id|field — what it returns" }
    fn run(&self, ctx: &mut ToolContext<'_>, call: &Call) -> Result<ToolResult> { ... }
}
```

2. Register it in `Registry::builtin`. The prompt menu and CLI `tools` update
   automatically.
3. Keep the output to compact rows; set `raw_chars` honestly.
4. Add a test in `crates/harness/tests/tools.rs` that asserts both the content
   and that resume bodies do not leak.

Tools receive a `ToolContext` with the store, the optional embedder, the handle
table, the budget, and a lazily loaded `Library` (metadata plus vectors). Use
`ctx.handle(&candidate)` for output and `ctx.candidate_id(raw)` to accept
handles or raw ids.

## Limits

- **Direct answers are opt-in.** `Config::direct` can render counts, profiles,
  and explicit lists locally for offline tooling and benchmarks. Chat leaves it
  off so every answer is model-written.
- **Retrieval is the ceiling.** The harness improves token efficiency, not
  recall; BM25 and e5 still decide which candidates are in the pool.
- **`find matches=` is exact only for query-only searches.** With structured
  filters it is the count in the evaluated pool; `stats` is always the exact
  library count.
- **Estimates, not invoices.** Token counts come from the local estimator and
  the provider may tokenise differently.
- **Chat privacy is unchanged.** Every chat question sends the model a compact
  profile or matched window, including names; resume bodies stay on disk.
- **Tool results are data.** The prompt labels them as such, but a resume is
  still untrusted input; model writes are disabled, so the worst case is a bad
  answer, not a changed record.
