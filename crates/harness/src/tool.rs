//! Tool definitions and the dispatch context.
//!
//! A tool is a small, deterministic local function with a one-line contract.
//! The model pays for the contract once per run and then only for the compact
//! result. Adding a tool means implementing [`Tool`] and registering it in
//! [`Registry::builtin`]; the prompt catalogue updates itself.

use anyhow::{Result, anyhow};
use shortlist_core::CandidateLite;
use shortlist_embed::Embedder;
use shortlist_store::Store;
use std::cell::OnceCell;
use std::collections::HashMap;

use crate::budget::Budget;
use crate::protocol::Call;
use crate::refs::Refs;
use crate::search::{Library, Ranked};

/// What a tool returns before the budget clips it.
pub struct ToolResult {
    pub body: String,
    /// Size of the naive payload this call replaced, for the savings report.
    pub raw_chars: usize,
    /// A short line for the UI progress feed.
    pub summary: String,
}

impl ToolResult {
    pub fn new(body: impl Into<String>, raw_chars: usize, summary: impl Into<String>) -> Self {
        Self {
            body: body.into(),
            raw_chars,
            summary: summary.into(),
        }
    }
}

pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;

    /// One line: `name args — what it returns`. This is the model's only
    /// documentation, so it must earn every token.
    fn signature(&self) -> &'static str;

    fn run(&self, ctx: &mut ToolContext<'_>, call: &Call) -> Result<ToolResult>;
}

/// State shared by every call in one run.
pub struct ToolContext<'a> {
    pub store: &'a Store,
    pub embedder: Option<&'a Embedder>,
    pub refs: Refs,
    pub budget: Budget,
    /// Model runs are read-only by default; the drawer owns note edits.
    pub allow_writes: bool,
    library: OnceCell<Library>,
    cache: HashMap<String, String>,
}

impl<'a> ToolContext<'a> {
    pub fn new(store: &'a Store, embedder: Option<&'a Embedder>, budget: Budget) -> Self {
        Self {
            store,
            embedder,
            refs: Refs::default(),
            budget,
            allow_writes: false,
            library: OnceCell::new(),
            cache: HashMap::new(),
        }
    }

    /// Allow note writes. Intended for the CLI, never for a model run.
    pub fn with_writes(mut self) -> Self {
        self.allow_writes = true;
        self
    }

    /// Load metadata and vectors once per run, on first use.
    pub fn library(&mut self) -> Result<&Library> {
        if self.library.get().is_none() {
            let library = Library::load(self.store)?;
            let _ = self.library.set(library);
        }
        self.library
            .get()
            .ok_or_else(|| anyhow!("candidate index unavailable"))
    }

    pub fn ranked(&mut self, query: &str, limit: usize) -> Result<Vec<Ranked>> {
        let store = self.store;
        let embedder = self.embedder;
        let library = self.library()?;
        library.ranked(store, embedder, query, limit)
    }

    pub fn handle(&mut self, candidate: &CandidateLite) -> String {
        self.refs.handle(candidate)
    }

    /// Resolve a handle (`a1`), a bracketed handle, or a raw candidate id.
    pub fn candidate_id(&self, raw: &str) -> Option<String> {
        if let Some(id) = self.refs.resolve(raw) {
            return Some(id.to_string());
        }
        let trimmed = raw
            .trim()
            .trim_start_matches(['@', '['])
            .trim_end_matches(']');
        let Some(library) = self.library.get() else {
            return None;
        };
        if let Some(candidate) = library.get(trimmed) {
            return Some(candidate.id.clone());
        }
        if let Some(candidate) = library
            .rows
            .iter()
            .find(|candidate| candidate.name.eq_ignore_ascii_case(trimmed))
        {
            return Some(candidate.id.clone());
        }
        // A unique first name is a convenient native-tool argument.
        let mut first_name_matches = library.rows.iter().filter(|candidate| {
            candidate
                .name
                .split_whitespace()
                .next()
                .is_some_and(|first| first.eq_ignore_ascii_case(trimmed))
        });
        let first = first_name_matches.next()?;
        if first_name_matches.next().is_none() {
            Some(first.id.clone())
        } else {
            None
        }
    }

    pub fn cache_get(&self, key: &str) -> Option<&String> {
        self.cache.get(key)
    }

    pub fn cache_put(&mut self, key: String, body: String) {
        self.cache.insert(key, body);
    }
}

pub struct Registry {
    tools: Vec<Box<dyn Tool>>,
}

impl Registry {
    pub fn builtin() -> Self {
        Self {
            tools: vec![
                Box::new(crate::tools::Stats),
                Box::new(crate::tools::Find),
                Box::new(crate::tools::Get),
                Box::new(crate::tools::Read),
                Box::new(crate::tools::Grep),
                Box::new(crate::tools::Note),
                Box::new(crate::tools::Shortlist),
                Box::new(crate::tools::Compare),
            ],
        }
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.tools.iter().map(|tool| tool.name()).collect()
    }

    /// The compact tool catalogue injected into the system prompt.
    pub fn catalog(&self) -> String {
        self.tools
            .iter()
            .map(|tool| format!("- {}", tool.signature()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Signatures only: argument shape, no descriptions or column legends.
    /// Column order is carried by each result's `cols:` line instead.
    pub fn menu(&self) -> String {
        self.tools
            .iter()
            .map(|tool| {
                let signature = tool.signature();
                let short = signature
                    .split(" — ")
                    .next()
                    .unwrap_or("")
                    .split(" → ")
                    .next()
                    .unwrap_or("")
                    .trim_start_matches("- ");
                short.to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn run(&self, ctx: &mut ToolContext<'_>, call: &Call) -> Result<ToolResult> {
        let tool = self
            .tools
            .iter()
            .find(|tool| tool.name() == call.tool)
            .ok_or_else(|| anyhow!("unknown tool '{}'", call.tool))?;
        tool.run(ctx, call)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_lists_every_tool() {
        let registry = Registry::builtin();
        let catalog = registry.catalog();
        for name in registry.names() {
            assert!(catalog.contains(name), "missing {name}");
        }
        assert!(catalog.lines().count() == registry.names().len());
    }
}
