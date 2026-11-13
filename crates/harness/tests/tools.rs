//! End-to-end tool behaviour, with no model in the loop.

use shortlist_harness::{Budget, Call, Registry, ToolContext};
use shortlist_store::Store;

fn store() -> Store {
    let store = Store::open_in_memory().unwrap();
    store.seed_demo().unwrap();
    store
}

fn call(tool: &str, args: &[(&str, &str)]) -> Call {
    Call {
        tool: tool.into(),
        args: args
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect(),
    }
}

fn run(store: &Store, tool: &str, args: &[(&str, &str)]) -> String {
    let registry = Registry::builtin();
    let mut ctx = ToolContext::new(store, None, Budget::new(8_000)).with_writes();
    registry
        .run(&mut ctx, &call(tool, args))
        .unwrap_or_else(|error| panic!("{tool} failed: {error}"))
        .body
}

#[test]
fn stats_summarises_the_library_without_resumes() {
    let store = store();
    let body = run(&store, "stats", &[("n", "5")]);
    assert!(body.contains("stats n=12"), "{body}");
    assert!(body.contains("Rust(3)"), "{body}");
    assert!(body.contains("files:"), "{body}");
    assert!(!body.contains("ledger service"));
}

#[test]
fn find_returns_compact_rows_and_respects_paging() {
    let store = store();
    let first = run(&store, "find", &[("q", "rust"), ("n", "10")]);
    assert!(first.contains("find rows=3"), "{first}");
    assert!(first.contains("Sarah Chen"), "{first}");
    assert!(first.contains("|"), "{first}");
    assert!(
        !first.contains("production systems"),
        "resume body leaked: {first}"
    );

    let page_one = run(&store, "find", &[("q", "rust"), ("n", "2")]);
    let page_two = run(&store, "find", &[("q", "rust"), ("n", "2"), ("p", "2")]);
    assert_ne!(page_one, page_two);
}

#[test]
fn shortlist_carries_score_columns_and_evidence() {
    let store = store();
    let body = run(
        &store,
        "shortlist",
        &[
            ("q", "payments infrastructure"),
            ("skills", "Rust"),
            ("miny", "5"),
            ("n", "5"),
        ],
    );
    assert!(body.contains("shortlist rows="), "{body}");
    assert!(body.contains("|"), "{body}");
    // Sarah is the Rust + payments demo profile.
    assert!(body.contains("Sarah Chen"), "{body}");
    assert!(body.lines().count() >= 2);
}

#[test]
fn get_is_a_profile_not_a_resume() {
    let store = store();
    let body = run(&store, "get", &[("id", "demo-01")]);
    assert!(body.contains("Sarah Chen"), "{body}");
    assert!(body.contains("y=8"), "{body}");
    assert!(body.contains("skills=Rust"), "{body}");
    assert!(!body.contains("text="), "resume body leaked: {body}");
    assert!(!body.contains("Experience"), "resume body leaked: {body}");
    assert!(body.len() < 800, "profile too large: {}", body.len());
}

#[test]
fn read_returns_only_matching_windows() {
    let store = store();
    let body = run(&store, "read", &[("id", "demo-01"), ("q", "latency")]);
    assert!(body.contains("hits="), "{body}");
    assert!(body.contains("latency"), "{body}");
    assert!(body.len() < 500, "{body}");

    let miss = run(&store, "read", &[("id", "demo-01"), ("q", "cobol")]);
    assert!(miss.contains("hits=0"), "{miss}");
}

#[test]
fn compare_aligns_a_few_candidates() {
    let store = store();
    let body = run(
        &store,
        "compare",
        &[("ids", "demo-01,demo-02"), ("f", "yrs,skills")],
    );
    assert!(body.contains("compare cols: id|name|yrs|skills"), "{body}");
    assert!(body.contains("Sarah Chen|8|"), "{body}");
    assert!(body.contains("Marcus Webb|11|"), "{body}");
}

#[test]
fn notes_round_trip_through_the_tool() {
    let store = store();
    let saved = run(
        &store,
        "note",
        &[("id", "demo-01"), ("set", "wants a principal role")],
    );
    assert!(saved.contains("saved"), "{saved}");
    let read = run(&store, "note", &[("id", "demo-01")]);
    assert!(read.contains("wants a principal role"), "{read}");
    let deleted = run(&store, "note", &[("id", "demo-01"), ("del", "1")]);
    assert!(deleted.contains("deleted"), "{deleted}");
    assert!(!run(&store, "note", &[("id", "demo-01")]).contains("principal"));
}

#[test]
fn handles_are_reused_across_calls_in_one_context() {
    let store = store();
    let registry = Registry::builtin();
    let mut ctx = ToolContext::new(&store, None, Budget::new(8_000));
    let find = registry
        .run(&mut ctx, &call("find", &[("q", "sarah")]))
        .unwrap();
    let handle = find
        .body
        .lines()
        .find(|line| line.starts_with('a'))
        .and_then(|line| line.split('|').next())
        .unwrap()
        .to_string();
    assert_eq!(handle, "a1");
    let get = registry
        .run(&mut ctx, &call("get", &[("id", &handle)]))
        .unwrap();
    assert!(get.body.contains("Sarah Chen"));
    assert_eq!(ctx.refs.entries().len(), 1);
}

#[test]
fn model_runs_cannot_write_notes() {
    let store = store();
    let registry = Registry::builtin();
    let mut ctx = ToolContext::new(&store, None, Budget::new(8_000));
    let error = match registry.run(
        &mut ctx,
        &call("note", &[("id", "demo-01"), ("set", "injected")]),
    ) {
        Ok(_) => panic!("a model run must not write notes"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("disabled"), "{error}");
    assert!(store.note("demo-01").unwrap().is_none());
}

#[test]
fn a_ten_row_shortlist_fits_the_default_cap() {
    let store = store();
    let registry = Registry::builtin();
    let mut ctx = ToolContext::new(&store, None, Budget::new(1_600));
    let result = registry
        .run(
            &mut ctx,
            &call("shortlist", &[("q", "engineer"), ("n", "10")]),
        )
        .unwrap();
    let mut budget = Budget::new(1_600);
    let body = result.body.clone();
    let allotment = budget.take(result.body, 360, result.raw_chars);
    assert!(!allotment.clipped, "{body}");
    assert!(allotment.tokens <= 360);
}

#[test]
fn the_whole_catalogue_is_cheap() {
    let registry = Registry::builtin();
    let catalog = registry.catalog();
    assert!(
        catalog.len() < 1_200,
        "catalogue is {} chars",
        catalog.len()
    );
    assert!(
        shortlist_harness::estimate_tokens(&catalog) < 350,
        "catalogue is {} tokens",
        shortlist_harness::estimate_tokens(&catalog)
    );
}
