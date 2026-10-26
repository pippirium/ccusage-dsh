use super::*;
use ccusage_core::cli::{AgentReportKind, CostMode};
use ccusage_test_support::fs_fixture;
use serde_json::{Value, json};

fn log(version: u64, session: &str, events: Vec<Value>) -> String {
    let mut rows = vec![
        json!({"type":"session", "version":version, "id":session, "cwd":"/synthetic/project"}),
        json!({"type":"request/header", "seq":1, "data":{"header":{"config":{"model":"synthetic-model"}}}}),
    ];
    rows.extend(events);
    rows.into_iter().map(|v| v.to_string() + "\n").collect()
}
fn event(seq: u64, usage: Value) -> Value {
    json!({"type":"assistant/message", "seq":seq, "time":1792940400000_i64, "data":{"usage":usage}})
}
fn shared() -> SharedArgs {
    SharedArgs {
        timezone: Some("Asia/Tokyo".into()),
        ..SharedArgs::default()
    }
}
fn parse(s: &str) -> Vec<LoadedEntry> {
    read_log(s.as_bytes(), &shared(), Path::new("fixture")).unwrap()
}

#[test]
fn committed_usage_overrides_duplicate_stream_and_disjoint_cache_is_counted_once() {
    let mut e = event(
        2,
        json!({"inputTokens":100,"outputTokens":20,"cacheReadTokens":900,"totalTokens":1020}),
    );
    e["data"]["stream"] = json!([{"chunk":{"type":"usage","usage":{"inputTokens":100,"outputTokens":20,"cacheReadTokens":900,"totalTokens":1020}}}]);
    for version in [3, 4] {
        let entries = parse(&log(version, "a", vec![e.clone()]));
        let rows = crate::summarize_entries(&entries, AgentReportKind::Daily).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].input_tokens, 100);
        assert_eq!(rows[0].cache_read_tokens, 900);
        assert_eq!(rows[0].output_tokens, 20);
        assert_eq!(rows[0].total_cost, 0.0);
        assert_eq!(rows[0].extra_total_tokens, 0);
        assert_eq!(entries[0].date, "2026-10-26");
    }
}
#[test]
fn retries_use_final_stream_usage_and_separate_attempts_are_billable() {
    let attempt = json!({"type":"assistant/attempt","seq":2,"time":1792940400000_i64,"data":{"stream":[
        {"chunk":{"type":"usage","usage":{"inputTokens":10,"outputTokens":1}}},
        {"chunk":{"type":"usage","usage":{"inputTokens":10,"outputTokens":2}}}
    ]}});
    let entries = parse(&log(
        4,
        "a",
        vec![
            attempt,
            event(3, json!({"inputTokens":10,"outputTokens":5})),
        ],
    ));
    assert_eq!(entries.len(), 2);
    let rows = crate::summarize_entries(&entries, AgentReportKind::Daily).unwrap();
    assert_eq!(rows[0].input_tokens, 20);
    assert_eq!(rows[0].output_tokens, 7);
}
#[test]
fn missing_usage_is_not_estimated_and_reported_total_remainder_is_preserved() {
    let entries = parse(&log(
        4,
        "a",
        vec![event(2, Value::Null), event(3, json!({"totalTokens":40}))],
    ));
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].extra_total_tokens, 40);
}
#[test]
fn model_changes_follow_request_headers() {
    let entries = parse(&log(
        4,
        "a",
        vec![
            event(2, json!({"inputTokens":1})),
            json!({"type":"request/header","data":{"header":{"config":{"model":"other-model"}}}}),
            event(3, json!({"inputTokens":2})),
        ],
    ));
    assert_eq!(entries[0].model.as_deref(), Some("synthetic-model"));
    assert_eq!(entries[1].model.as_deref(), Some("other-model"));
}
#[test]
fn invalid_complete_record_is_reported_and_incomplete_tail_is_ignored() {
    let s = log(4, "a", vec![event(2, json!({"inputTokens":1}))]);
    assert_eq!(parse(&(s.clone() + "{\"type\":")).len(), 1);
    assert!(
        read_log(
            (s + "invalid\n").as_bytes(),
            &shared(),
            Path::new("fixture")
        )
        .is_err()
    );
}
#[test]
fn compressed_discovery_migration_copy_dedupe_and_child_sessions() {
    let fixture = fs_fixture!({"empty":""});
    let root = fixture.root();
    let a = log(
        4,
        "parent",
        vec![event(2, json!({"inputTokens":10,"outputTokens":5}))],
    );
    let b = log(
        3,
        "child",
        vec![event(2, json!({"inputTokens":20,"outputTokens":6}))],
    );
    for (path, contents) in [
        ("project/parent/session.v4.jsonl.zstd", a.as_str()),
        ("copy/parent/session.v4.jsonl.zstd", a.as_str()),
        ("project/child/session.v3.jsonl.zstd", b.as_str()),
        ("project/parent/session.v3.jsonl.zstd", b.as_str()),
    ] {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            zstd::stream::encode_all(contents.as_bytes(), 1).unwrap(),
        )
        .unwrap();
    }
    assert_eq!(discover(root).unwrap().len(), 3);
    for mode in [CostMode::Auto, CostMode::Calculate, CostMode::Display] {
        let mut args = shared();
        args.mode = mode;
        let entries = load_from_root(root, &args).unwrap();
        assert_eq!(entries.len(), 2);
        for kind in [
            AgentReportKind::Daily,
            AgentReportKind::Weekly,
            AgentReportKind::Monthly,
            AgentReportKind::Session,
        ] {
            let rows = crate::summarize_entries(&entries, kind).unwrap();
            assert_eq!(rows.iter().map(|r| r.input_tokens).sum::<u64>(), 30);
            assert_eq!(rows.iter().map(|r| r.output_tokens).sum::<u64>(), 11);
            assert_eq!(rows.iter().map(|r| r.total_cost).sum::<f64>(), 0.0);
        }
    }
}
#[test]
fn unknown_format_is_an_error_instead_of_silent_undercount() {
    let fixture = fs_fixture!({"project/session.v5.jsonl.zstd":""});
    assert!(discover(fixture.root()).is_err());
}
#[test]
fn conflicting_copies_are_rejected() {
    let fixture = fs_fixture!({"empty":""});
    for (dir, n) in [("a", 10), ("b", 20)] {
        let path = fixture.root().join(dir);
        fs::create_dir_all(&path).unwrap();
        fs::write(
            path.join("session.v4.jsonl.zstd"),
            zstd::stream::encode_all(
                log(4, "same", vec![event(2, json!({"inputTokens":n}))]).as_bytes(),
                1,
            )
            .unwrap(),
        )
        .unwrap();
    }
    assert!(load_from_root(fixture.root(), &shared()).is_err());
}
#[test]
fn forked_history_is_excluded_but_original_history_is_counted() {
    let s = log(
        4,
        "fork",
        vec![
            event(2, json!({"inputTokens":100})),
            json!({"type":"session/end-seed","seq":3}),
            event(4, json!({"inputTokens":10})),
        ],
    );
    assert_eq!(parse(&s).len(), 2);
    let seeded = s.replacen("\"version\":4", "\"version\":4,\"isSeeded\":true", 1);
    let entries = parse(&seeded);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].data.message.usage.input_tokens, 10);
    assert_eq!(entries[0].model.as_deref(), Some("synthetic-model"));
}

#[test]
fn repeated_settlement_replaces_usage_until_a_retry_boundary() {
    let first = json!({"type":"assistant/message","seq":2,"time":1792940400000_i64,"data":{"turn":1,"step":1,"usage":{"inputTokens":10,"outputTokens":1}}});
    let replacement = json!({"type":"assistant/message","seq":3,"time":1792940400000_i64,"data":{"turn":1,"step":1,"usage":{"inputTokens":10,"outputTokens":2}}});
    let retry = json!({"type":"llm/retry-started","seq":4,"data":{"turn":1,"step":1}});
    let retried = json!({"type":"assistant/attempt","seq":5,"time":1792940400000_i64,"data":{"turn":1,"step":1,"stream":[{"chunk":{"type":"usage","usage":{"inputTokens":20,"outputTokens":3}}}]}});
    let entries = parse(&log(4, "a", vec![first, replacement, retry, retried]));
    let rows = crate::summarize_entries(&entries, AgentReportKind::Daily).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(rows[0].input_tokens, 30);
    assert_eq!(rows[0].output_tokens, 5);
}
