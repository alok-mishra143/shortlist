//! The built-in tool set.

mod detail;
mod overview;

pub use detail::{Compare, Get, Grep, Note, Read};
pub use overview::{Find, Shortlist, Stats};

use anyhow::{Result, anyhow};

use crate::protocol::Call;
use crate::tool::ToolContext;
use shortlist_core::CandidateLite;

/// A resolved candidate: raw id, short handle, and metadata.
pub struct Resolved {
    pub id: String,
    pub handle: String,
    pub row: CandidateLite,
}

pub fn resolve(ctx: &mut ToolContext<'_>, call: &Call, key: &str) -> Result<Resolved> {
    let raw = call
        .arg(key)
        .ok_or_else(|| anyhow!("{} needs {key}=a1", call.tool))?;
    resolve_id(ctx, raw)
}

pub fn resolve_id(ctx: &mut ToolContext<'_>, raw: &str) -> Result<Resolved> {
    ctx.library()?;
    let id = ctx
        .candidate_id(raw)
        .ok_or_else(|| anyhow!("unknown candidate '{raw}'. Use a handle from find/shortlist"))?;
    let row = ctx
        .library()?
        .get(&id)
        .cloned()
        .ok_or_else(|| anyhow!("candidate not indexed"))?;
    let handle = ctx.handle(&row);
    Ok(Resolved { id, handle, row })
}

pub fn text_of(ctx: &ToolContext<'_>, id: &str) -> Result<String> {
    ctx.store
        .candidate(id)?
        .map(|candidate| candidate.text)
        .ok_or_else(|| anyhow!("candidate body unavailable"))
}

pub fn count(value: Option<usize>, default: usize, max: usize) -> usize {
    value.unwrap_or(default).clamp(1, max)
}
