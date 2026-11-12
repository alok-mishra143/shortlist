<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/brand/hero-dark.svg">
  <img src="assets/brand/hero-light.svg" alt="Shortlist — ten candidates, every one defensible" width="100%">
</picture>

<br>

![Status](https://img.shields.io/badge/Status-early%20build-B45309?style=flat-square)
![Rust](https://img.shields.io/badge/Rust-2024-000000?style=flat-square&logo=rust&logoColor=white)
![GPUI](https://img.shields.io/badge/UI-GPUI%20Kit%200.9.1-14532D?style=flat-square)
![License](https://img.shields.io/badge/License-Apache--2.0-6B7280?style=flat-square)
<br>
![SQLite](https://img.shields.io/badge/Store-SQLite%20%C2%B7%20FTS5-003B57?style=flat-square&logo=sqlite&logoColor=white)
![Embeddings](https://img.shields.io/badge/Embeddings-e5--small--v2-374151?style=flat-square)
![Platforms](https://img.shields.io/badge/Platforms-macOS%20%7C%20Windows%20%7C%20Linux-14130F?style=flat-square)

**[Quick start](#quick-start)** &nbsp;·&nbsp; **[What's inside](#whats-inside)** &nbsp;·&nbsp; **[Under the hood](#under-the-hood)** &nbsp;·&nbsp; **[Roadmap](#roadmap)** &nbsp;·&nbsp; **[Full spec](docs/spec.md)**

</div>

**Drop in 2,000 resumes. Paste a job description. Read ten candidates — each with a score you can defend and evidence you can read.**

| **2,000+** | **< 3 s** | **33 MB** | **0** |
|:--:|:--:|:--:|:--:|
| resumes per library | local first pass | embedding encoder | webviews |

> [!NOTE]
> **Early build.** Import, search, local embeddings, and grounded chat run on macOS today. JD → Top 10 is next.

## Three steps

| **01 · Drop in** | **02 · Paste the job** | **03 · Read ten** |
|:--:|:--:|:--:|
| PDF, DOCX, TXT.<br>One file or a whole folder. | Job description in.<br>Editable, weighted criteria out. | Ten names, ranked.<br>A score, evidence, and gaps. |

## What's inside

| Surface | What it does | |
|:--|:--|:--:|
| **Import** | Drop a folder. Every resume is searchable in about a second. | ✅ |
| **Candidates** | Searchable table, candidate drawer, rich notes, dark mode. | ✅ |
| **Chat** | Ask the library. Answers cite candidates as inline chips. | ✅ |
| **Job** | A JD becomes visible, editable, weighted criteria. | 🚧 |
| **Top 10** | Ten candidates, scored and explained. | 🚧 |
| **Report** | CSV or a clean PDF for the client. | 🚧 |

## Quick start

```bash
./scripts/fetch-model.sh            # once · 33 MB encoder
./scripts/run.sh                    # build + open
SHORTLIST_DEMO=1 ./scripts/run.sh   # demo library
```

Rust stable · macOS 13+ · [OpenCode](https://opencode.ai) for AI features.

## Under the hood

```mermaid
flowchart LR
    A["2,000 resumes"] --> B["BM25 + vectors<br/>local · under 3 s"]
    B --> C["Top 30"]
    C --> D["Model judge<br/>quoted evidence"]
    D --> E["Top 10"]
```

Stage 1 is local and cheap. Stage 2 reads only thirty candidates — that is why a run takes a minute, not hours. [How it works →](docs/spec.md#6-how-the-ai-works)

## Why local

| | Cloud screening | **Shortlist** |
|:--|:--|:--|
| Candidate data | Uploaded to a vendor | **Stays on your disk** |
| The score | An opaque number | **Quoted evidence and gaps** |
| The model | Their choice | **Your existing OpenCode** |
| Offline | No | **Yes, except judging** |

## Roadmap

| ✅ Working today | 🟡 In progress | ⬜ Next |
|:--|:--|:--|
| Import + parsing | Virtualized table | JD → Top 10 |
| Local embeddings | Windows + Linux | Accuracy harness |
| Hybrid search | | PDF viewer |
| Grounded chat | | Packaging |
| Candidate drawer | | |
| Rich-text notes | | |
| Dark mode | | |

<div align="center">

**[Read the full spec →](docs/spec.md)**

<sub>Shortlist · Apache-2.0 · Rust + GPUI</sub>

</div>
