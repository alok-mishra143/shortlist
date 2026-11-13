//! Library-wide tools: overview, search, and shortlisting.

use anyhow::Result;

use super::count;
use crate::protocol::Call;
use crate::search::{Criteria, Fit, compact_list, evaluate, snippet, years};
use crate::tool::{Tool, ToolContext, ToolResult};

pub struct Stats;

impl Tool for Stats {
    fn name(&self) -> &'static str {
        "stats"
    }

    fn signature(&self) -> &'static str {
        "stats [n] — counts, top skills, locations"
    }

    fn run(&self, ctx: &mut ToolContext<'_>, call: &Call) -> Result<ToolResult> {
        let top = count(call.uint("n"), 12, 40);
        let (total, min, max, average, skills, locations, files) = {
            let library = ctx.library()?;
            let rows = &library.rows;
            if rows.is_empty() {
                (0, 0.0, 0.0, 0.0, Vec::new(), Vec::new(), Vec::new())
            } else {
                let mut min = f32::MAX;
                let mut max = 0.0f32;
                let mut sum = 0.0f32;
                let mut skill_counts: std::collections::HashMap<String, (usize, String)> =
                    std::collections::HashMap::new();
                let mut location_counts: std::collections::HashMap<String, usize> =
                    std::collections::HashMap::new();
                let mut file_counts: std::collections::HashMap<String, usize> =
                    std::collections::HashMap::new();
                for row in rows {
                    min = min.min(row.years);
                    max = max.max(row.years);
                    sum += row.years;
                    for skill in &row.skills {
                        let entry = skill_counts
                            .entry(skill.to_lowercase())
                            .or_insert((0, skill.clone()));
                        entry.0 += 1;
                    }
                    if !row.location.trim().is_empty() {
                        *location_counts.entry(row.location.clone()).or_default() += 1;
                    }
                    if let Some(extension) = row.file_name.rsplit('.').next()
                        && extension.len() <= 5
                    {
                        *file_counts.entry(extension.to_lowercase()).or_default() += 1;
                    }
                }
                let mut skills: Vec<(usize, String)> = skill_counts.into_values().collect();
                skills.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
                let skills = skills
                    .into_iter()
                    .take(top)
                    .map(|(count, name)| format!("{name}({count})"))
                    .collect::<Vec<_>>();
                let mut locations: Vec<(usize, String)> = location_counts
                    .into_iter()
                    .map(|(name, count)| (count, name))
                    .collect();
                locations.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
                let locations = locations
                    .into_iter()
                    .take(top.min(8))
                    .map(|(count, name)| format!("{name}({count})"))
                    .collect::<Vec<_>>();
                let mut files: Vec<(usize, String)> = file_counts
                    .into_iter()
                    .map(|(name, count)| (count, name))
                    .collect();
                files.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
                let files = files
                    .into_iter()
                    .take(6)
                    .map(|(count, name)| format!("{name}:{count}"))
                    .collect::<Vec<_>>();
                (
                    rows.len(),
                    min,
                    max,
                    sum / rows.len() as f32,
                    skills,
                    locations,
                    files,
                )
            }
        };

        if total == 0 {
            return Ok(ToolResult::new(
                "stats n=0 — the candidate library is empty",
                ctx.store.total_text_chars()?,
                "empty",
            ));
        }
        let mut body = format!(
            "stats n={total} y={}-{} avg={:.1}",
            years(min),
            years(max),
            average
        );
        if !skills.is_empty() {
            body.push_str(&format!("\nskills: {}", skills.join(",")));
        }
        if !locations.is_empty() {
            body.push_str(&format!("\nloc: {}", locations.join(";")));
        }
        if !files.is_empty() {
            body.push_str(&format!("\nfiles: {}", files.join(",")));
        }
        Ok(ToolResult::new(
            body,
            ctx.store.total_text_chars()?,
            format!("{total} candidates"),
        ))
    }
}

pub struct Find;

impl Tool for Find {
    fn name(&self) -> &'static str {
        "find"
    }

    fn signature(&self) -> &'static str {
        "find q [skills any miny loc n p sort] → id|name|title|yrs|loc|skills — search"
    }

    fn run(&self, ctx: &mut ToolContext<'_>, call: &Call) -> Result<ToolResult> {
        let n = count(call.uint("n"), 8, 30);
        let page = call.uint("p").unwrap_or(1).max(1);
        let sort = call.arg("sort").unwrap_or("rel").to_ascii_lowercase();
        let criteria = Criteria::from_call(call);

        // `find` is the deterministic browse/search table: lexical only, so its
        // match count is exact. `shortlist` is the semantic ranker.
        let candidates = if criteria.query.is_empty() {
            ctx.store.list_lite(100_000, 0)?
        } else {
            ctx.store.search_lite(&criteria.query, 240)?
        };
        let lexical_count = candidates.len();
        let mut matches: Vec<(shortlist_core::CandidateLite, f32)> = candidates
            .into_iter()
            .enumerate()
            .filter_map(|(rank, row)| {
                let relevance = if lexical_count <= 1 {
                    1.0
                } else {
                    1.0 - rank as f32 / lexical_count as f32
                };
                evaluate(&row, &criteria, relevance).map(|fit| (row, fit.score))
            })
            .collect();

        match sort.as_str() {
            "yrs" | "years" => matches.sort_by(|a, b| {
                b.0.years
                    .partial_cmp(&a.0.years)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            "name" => matches.sort_by(|a, b| a.0.name.cmp(&b.0.name)),
            _ => {}
        }

        let available = matches.len();
        let start = (page - 1) * n;
        let selected: Vec<(shortlist_core::CandidateLite, f32)> =
            matches.into_iter().skip(start).take(n).collect();
        if selected.is_empty() {
            return Ok(ToolResult::new(
                format!("find rows=0 (page {page}; only {available} matches)"),
                ctx.store.total_text_chars()?,
                "0 rows",
            ));
        }

        let mut lines = Vec::with_capacity(selected.len());
        for (row, _score) in &selected {
            let handle = ctx.handle(row);
            lines.push(format!(
                "{}|{}|{}|{}|{}|{}",
                handle,
                field(&row.name, 32),
                field(&row.headline, 46),
                years(row.years),
                field(&row.location, 30),
                compact_list(&row.skills, 6)
            ));
        }
        let more = if start + selected.len() < available {
            " more=1"
        } else {
            ""
        };
        let structured = !criteria.require.is_empty()
            || !criteria.any.is_empty()
            || criteria.min_years.is_some()
            || criteria.max_years.is_some()
            || criteria.location.is_some();
        let matches = if structured {
            available
        } else {
            ctx.store.count_matching(&criteria.query)?
        };
        let body = format!(
            "find rows={} matches={matches} library={}{more}\ncols: id|name|title|yrs|loc|skills\n{}",
            selected.len(),
            ctx.store.count()?,
            lines.join("\n")
        );
        Ok(ToolResult::new(
            body,
            ctx.store.total_text_chars()?,
            format!("{} rows", selected.len()),
        ))
    }
}

pub struct Shortlist;

impl Tool for Shortlist {
    fn name(&self) -> &'static str {
        "shortlist"
    }

    fn signature(&self) -> &'static str {
        "shortlist q [skills any miny loc n] → id|name|pre|req|any|yrs|loc|skills|why — screen"
    }

    fn run(&self, ctx: &mut ToolContext<'_>, call: &Call) -> Result<ToolResult> {
        let n = count(call.uint("n"), 10, 25);
        let criteria = Criteria::from_call(call);
        if criteria.is_empty() {
            return Ok(ToolResult::new(
                "shortlist needs q= or skills= (use find to browse)",
                0,
                "needs criteria",
            ));
        }
        let limit = if criteria.query.is_empty() {
            100_000
        } else {
            160
        };
        let hits = ctx.ranked(&criteria.query, limit)?;
        let mut ranked: Vec<(shortlist_core::CandidateLite, Fit, String)> = {
            let library = ctx.library()?;
            hits.into_iter()
                .filter_map(|hit| {
                    let row = library.rows.get(hit.row)?;
                    evaluate(row, &criteria, hit.relevance)
                        .map(|fit| (row.clone(), fit, hit.snippet))
                })
                .collect()
        };
        ranked.sort_by(|a, b| {
            let gap = (a.1.score - b.1.score).abs();
            if gap < 0.05 {
                b.0.years
                    .partial_cmp(&a.0.years)
                    .unwrap_or(std::cmp::Ordering::Equal)
            } else {
                b.1.score
                    .partial_cmp(&a.1.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }
        });
        let total = ranked.len();
        let selected: Vec<(shortlist_core::CandidateLite, Fit, String)> =
            ranked.into_iter().take(n).collect();
        if selected.is_empty() {
            return Ok(ToolResult::new(
                "shortlist rows=0 (no candidate passes miny/loc; relax the filters)",
                ctx.store.total_text_chars()?,
                "0 rows",
            ));
        }

        let partial = selected
            .iter()
            .filter(|(_, fit, _)| !fit.missing.is_empty())
            .count();
        let mut lines = Vec::with_capacity(selected.len());
        for (row, fit, why) in &selected {
            let handle = ctx.handle(row);
            let evidence = if n <= 5 {
                snippet(career_evidence(row, why), 64).replace('|', "/")
            } else {
                String::new()
            };
            lines.push(format!(
                "{}|{}|{}|{}|{}|{}|{}|{}|{}",
                handle,
                field(&row.name, 32),
                fit.score.round() as i64,
                ratio(fit.require_hits, fit.require_total, &fit.missing),
                ratio(fit.any_hits, fit.any_total, &[]),
                years(row.years),
                field(&row.location, 24),
                compact_list(&row.skills, 4).replace('|', "/"),
                evidence
            ));
        }
        let matches = total;
        let header = if partial > 0 {
            format!(
                "shortlist rows={} evaluated={matches} library={} partial={partial} (req column shows missing after !)\ncols: id|name|pre|req|any|yrs|loc|skills|why",
                selected.len(),
                ctx.store.count()?
            )
        } else {
            format!(
                "shortlist rows={} evaluated={matches} library={}\ncols: id|name|pre|req|any|yrs|loc|skills|why",
                selected.len(),
                ctx.store.count()?
            )
        };
        let body = format!("{header}\n{}", lines.join("\n"));
        Ok(ToolResult::new(
            body,
            ctx.store.total_text_chars()?,
            format!("{} rows", selected.len()),
        ))
    }
}

fn ratio(hits: usize, total: usize, missing: &[String]) -> String {
    if total == 0 {
        return "-".into();
    }
    if missing.is_empty() {
        format!("{hits}/{total}")
    } else {
        format!("{hits}/{total}!{}", compact_list(missing, 3))
    }
}

/// Prefer a current role's description as evidence; fall back to the retrieved
/// resume chunk when the profile has no structured roles.
fn career_evidence<'a>(row: &'a shortlist_core::CandidateLite, fallback: &'a str) -> &'a str {
    row.experience
        .iter()
        .find(|role| role.is_current())
        .or_else(|| row.experience.first())
        .map(|role| role.description.trim())
        .filter(|description| !description.is_empty())
        .unwrap_or(fallback)
}

/// Collapse, clip, and protect the `|` column boundary.
fn field(value: &str, max_chars: usize) -> String {
    snippet(value, max_chars).replace('|', "/")
}
