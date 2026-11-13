//! Token-lean tool harness for the candidate library.
//!
//! The default chat path used to paste resume excerpts into one prompt. That
//! works, but its context grows with the library and with every follow-up.
//! This crate inverts it:
//!
//! 1. The model receives a compact tool catalogue and a one-line library
//!    digest, not resumes.
//! 2. It asks for exactly what it needs with a line-oriented call block.
//! 3. Tools run locally, in one batch per round trip, over metadata and the
//!    local vector index. They return compact rows with short handles (`a1`).
//! 4. Every result is capped and the whole turn has a budget.
//!
//! Typical screening: ~1,500 prompt tokens for a library of thousands, instead
//! of one excerpt block per question. The tools themselves are ordinary Rust
//! and can be called without a model — see [`Harness::catalog`] and
//! [`Registry`].
//!
//! ```no_run
//! use shortlist_harness::{Config, Harness, OpenCodeModel, RunOptions};
//! # fn run(store: &shortlist_store::Store) -> anyhow::Result<()> {
//! let harness = Harness::new(store, None, Config::default());
//! let report = harness.run(
//!     &OpenCodeModel::new(None),
//!     "Who has the strongest Rust background?",
//!     RunOptions::default(),
//! )?;
//! println!("{}", report.answer);
//! # Ok(()) }
//! ```

mod answer;
mod budget;
mod protocol;
mod refs;
mod run;
mod search;
mod tool;
mod tools;

pub use budget::{Budget, estimate_tokens};
pub use protocol::{Call, clean_answer, is_call_line, parse_calls};
pub use refs::{RefEntry, Refs};
pub use run::{ChatModel, Config, Harness, OpenCodeModel, RunOptions, RunReport};
pub use search::{Criteria, Fit, Library, Ranked, evaluate, skill_matches};
pub use tool::{Registry, Tool, ToolContext, ToolResult};
