use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::Arc,
};

use jiff::tz::TimeZone as JiffTimeZone;
use serde::Deserialize;

use ccusage_adapter_common::read_files_parallel;
use ccusage_core::*;

use crate::{Result, cli::SharedArgs, debug_log, parse_tz};

use super::paths::db_paths;

const REQUIRED_NODE_COLUMNS: &[&str] = &["session_id", "created_at", "chat_message"];

struct SessionInfo {
    working_directory: Option<String>,
    model: Option<String>,
}

pub fn load_entries(shared: &SharedArgs) -> Result<Vec<LoadedEntry>> {
    crate::progress::track_usage_load(
        crate::progress::UsageLoadAgent("Devin"),
        shared.json,
        || load_entries_inner(shared),
    )
}

fn load_entries_inner(shared: &SharedArgs) -> Result<Vec<LoadedEntry>> {
    let tz = parse_tz(shared.timezone.as_deref());
    let db_paths = db_paths(shared);
    let loaded = read_files_parallel(&db_paths, shared.single_thread, |db_path| {
        load_entries_from_database(db_path, tz.as_ref(), shared)
    });
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    for db_entries in loaded {
        for entry in db_entries {
            let id = entry
                .data
                .message
                .id
                .as_deref()
                .expect("Devin entries always have message IDs");
            if seen.insert(id.to_string()) {
                entries.push(entry);
            }
        }
    }
    entries.sort_by_key(|entry| entry.timestamp);
    Ok(entries)
}

fn load_entries_from_database(
    db_path: &Path,
    tz: Option<&JiffTimeZone>,
    shared: &SharedArgs,
) -> Vec<LoadedEntry> {
    let Ok(connection) =
        sqlite::Connection::open_with_flags(db_path, sqlite::OpenFlags::new().with_read_only())
    else {
        debug_log(
            shared,
            format!("Failed to open Devin database: {}", db_path.display()),
        );
        return Vec::new();
    };
    for pragma in ["PRAGMA busy_timeout = 5000", "PRAGMA query_only = ON"] {
        if let Err(error) = connection.execute(pragma) {
            debug_log(
                shared,
                format!(
                    "Failed to configure Devin database {}: {error}",
                    db_path.display()
                ),
            );
            return Vec::new();
        }
    }

    let node_columns = match table_columns(&connection, "message_nodes") {
        Ok(columns) => columns,
        Err(error) => {
            debug_log(
                shared,
                format!(
                    "Failed to inspect Devin message_nodes schema at {}: {error}",
                    db_path.display()
                ),
            );
            return Vec::new();
        }
    };
    let missing = missing_columns(&node_columns, REQUIRED_NODE_COLUMNS);
    if !missing.is_empty() {
        debug_log(
            shared,
            format!(
                "Unsupported Devin SQLite schema at {}: missing message_nodes: {}",
                db_path.display(),
                missing.join(", ")
            ),
        );
        return Vec::new();
    }

    let sessions = read_sessions(&connection, db_path, shared);

    // The bundled SQLite has no JSON1 functions, so assistant rows are
    // prefiltered with LIKE and chat_message is parsed in Rust.
    let query = "SELECT n.session_id, n.row_id, n.created_at, n.chat_message \
        FROM message_nodes n \
        WHERE n.chat_message LIKE '%\"role\"%assistant%' \
        ORDER BY n.row_id";
    let Ok(mut statement) = connection.prepare(query) else {
        debug_log(
            shared,
            format!("Failed to query Devin database: {}", db_path.display()),
        );
        return Vec::new();
    };

    // Streaming snapshots commit the same message several times under one
    // request_id; keep the last row so each settled request counts once.
    let mut entries: Vec<LoadedEntry> = Vec::new();
    let mut slots: HashMap<(String, String), usize> = HashMap::new();
    loop {
        match statement.next() {
            Ok(sqlite::State::Row) => {
                let Ok(session_id) = statement.read::<String, _>(0) else {
                    continue;
                };
                let row_id = statement.read::<i64, _>(1).unwrap_or(0);
                let Some(created_at) = read_timestamp_ms(&statement, 2) else {
                    continue;
                };
                let Ok(chat_message) = statement.read::<String, _>(3) else {
                    continue;
                };
                if let Some((dedup_id, entry)) =
                    parse_usage_row(session_id, row_id, created_at, &chat_message, &sessions, tz)
                {
                    let dedup_key = (entry.session_id.to_string(), dedup_id);
                    match slots.get(&dedup_key) {
                        Some(&index) => entries[index] = entry,
                        None => {
                            slots.insert(dedup_key, entries.len());
                            entries.push(entry);
                        }
                    }
                }
            }
            Ok(sqlite::State::Done) => break,
            Err(error) => {
                debug_log(
                    shared,
                    format!(
                        "Failed to read Devin database {}: {error}",
                        db_path.display()
                    ),
                );
                break;
            }
        }
    }
    entries
}

fn table_columns(connection: &sqlite::Connection, table: &str) -> sqlite::Result<HashSet<String>> {
    // LIMIT 0 obtains column metadata without materializing row values,
    // including prompt or content fields present in chat_message.
    let query = format!("SELECT * FROM \"{table}\" LIMIT 0");
    let statement = connection.prepare(query)?;
    Ok(statement.column_names().iter().cloned().collect())
}

fn missing_columns(columns: &HashSet<String>, required: &[&str]) -> Vec<String> {
    required
        .iter()
        .filter(|column| !columns.contains(**column))
        .map(|column| (*column).to_string())
        .collect()
}

fn read_sessions(
    connection: &sqlite::Connection,
    db_path: &Path,
    shared: &SharedArgs,
) -> HashMap<String, SessionInfo> {
    let mut sessions = HashMap::new();
    let Ok(mut statement) = connection.prepare("SELECT id, working_directory, model FROM sessions")
    else {
        debug_log(
            shared,
            format!(
                "Devin database has no readable sessions table: {}",
                db_path.display()
            ),
        );
        return sessions;
    };
    while let Ok(sqlite::State::Row) = statement.next() {
        let Ok(id) = statement.read::<String, _>(0) else {
            continue;
        };
        sessions.insert(
            id,
            SessionInfo {
                working_directory: statement.read::<Option<String>, _>(1).ok().flatten(),
                model: statement.read::<Option<String>, _>(2).ok().flatten(),
            },
        );
    }
    sessions
}

#[derive(Deserialize)]
struct Metrics {
    // Fields are null rather than absent when Devin does not report them.
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_read_tokens: Option<u64>,
    cache_creation_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct MessageMetadata {
    request_id: Option<String>,
    generation_model: Option<String>,
    metrics: Option<Metrics>,
}

#[derive(Deserialize)]
struct ChatMessage {
    role: Option<String>,
    message_id: Option<String>,
    metadata: Option<MessageMetadata>,
}

fn parse_usage_row(
    session_id: String,
    row_id: i64,
    created_at: i64,
    chat_message: &str,
    sessions: &HashMap<String, SessionInfo>,
    tz: Option<&JiffTimeZone>,
) -> Option<(String, LoadedEntry)> {
    let message: ChatMessage = serde_json::from_str(chat_message).ok()?;
    if message.role.as_deref() != Some("assistant") {
        return None;
    }
    let metrics = message.metadata.as_ref()?.metrics.as_ref()?;
    let tokens = TokenUsageRaw {
        input_tokens: metrics.input_tokens.unwrap_or(0),
        output_tokens: metrics.output_tokens.unwrap_or(0),
        cache_read_input_tokens: metrics.cache_read_tokens.unwrap_or(0),
        cache_creation_input_tokens: metrics.cache_creation_tokens.unwrap_or(0),
        speed: None,
        cache_creation: None,
    };
    if total_usage_tokens(tokens) == 0 {
        return None;
    }
    let metadata = message.metadata?;
    let request_id = metadata.request_id;
    let generation_model = metadata.generation_model;
    let dedup_id = request_id
        .clone()
        .or(message.message_id)
        .unwrap_or_else(|| format!("node:{row_id}"));
    let timestamp = TimestampMs::from_millis(created_at);
    let session = sessions.get(&session_id);
    let model = generation_model
        .or_else(|| session.and_then(|info| info.model.clone()))
        .unwrap_or_else(|| "devin-unknown".to_owned());
    let project_path = session
        .and_then(|info| info.working_directory.clone())
        .unwrap_or_else(|| "unknown".to_owned());
    let entry = LoadedEntry {
        data: UsageEntry {
            session_id: Some(session_id.clone()),
            timestamp: format_rfc3339_millis(timestamp),
            version: None,
            message: UsageMessage {
                usage: tokens,
                model: Some(model.clone()),
                id: Some(format!("devin:{session_id}:{dedup_id}")),
            },
            cost_usd: Some(0.0),
            request_id,
            is_api_error_message: None,
            is_sidechain: None,
        },
        timestamp,
        date: format_date_tz(timestamp, tz),
        project: Arc::from("devin"),
        session_id: Arc::from(session_id.as_str()),
        project_path: Arc::from(project_path.as_str()),
        cost: 0.0,
        credits: None,
        model: Some(model),
        message_count: Some(1),
        usage_limit_reset_time: None,
        missing_pricing_model: None,
        extra_total_tokens: 0,
        stored_cost: Some(0.0),
    };
    Some((dedup_id, entry))
}

fn read_timestamp_ms(statement: &sqlite::Statement<'_>, index: usize) -> Option<i64> {
    let value = statement.read::<i64, _>(index).ok()?;
    // Devin records epoch seconds; accept millisecond-scale values defensively.
    Some(if value > 100_000_000_000 {
        value
    } else {
        value.saturating_mul(1000)
    })
}

#[cfg(test)]
mod tests {
    use ccusage_test_support::{EnvVarGuard, fs_fixture};

    use super::*;
    use crate::{cli::CostMode, paths::DEVIN_HOME_ENV};

    fn create_db(path: &Path) {
        let db = sqlite::open(path).unwrap();
        db.execute(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                working_directory TEXT,
                model TEXT,
                created_at INTEGER,
                last_activity_at INTEGER
            )",
        )
        .unwrap();
        db.execute(
            "CREATE TABLE message_nodes (
                row_id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT,
                node_id INTEGER,
                parent_node_id INTEGER,
                chat_message TEXT,
                created_at INTEGER
            )",
        )
        .unwrap();
    }

    fn insert_session(db: &sqlite::Connection, id: &str, dir: &str, model: &str) {
        let mut statement = db
            .prepare("INSERT INTO sessions VALUES (?1, ?2, ?3, 1793600000, 1793600100)")
            .unwrap();
        statement.bind((1, id)).unwrap();
        statement.bind((2, dir)).unwrap();
        statement.bind((3, model)).unwrap();
        statement.next().unwrap();
    }

    fn assistant_message(request_id: &str, model: Option<&str>, metrics: &str) -> String {
        let metrics: serde_json::Value = serde_json::from_str(&format!("{{{metrics}}}")).unwrap();
        serde_json::json!({
            "message_id": format!("msg-{request_id}"),
            "role": "assistant",
            "content": "ok",
            "metadata": {
                "request_id": request_id,
                "generation_model": model,
                "metrics": metrics,
            }
        })
        .to_string()
    }

    fn insert_node(
        db: &sqlite::Connection,
        session_id: &str,
        node_id: i64,
        chat_message: &str,
        created_at: i64,
    ) {
        let mut statement = db
            .prepare("INSERT INTO message_nodes (session_id, node_id, chat_message, created_at) VALUES (?1, ?2, ?3, ?4)")
            .unwrap();
        statement.bind((1, session_id)).unwrap();
        statement.bind((2, node_id)).unwrap();
        statement.bind((3, chat_message)).unwrap();
        statement.bind((4, created_at)).unwrap();
        statement.next().unwrap();
    }

    fn load(root: &Path) -> Vec<LoadedEntry> {
        let _guard = EnvVarGuard::set(DEVIN_HOME_ENV, root);
        let shared = SharedArgs {
            mode: CostMode::Display,
            timezone: Some("UTC".to_string()),
            ..SharedArgs::default()
        };
        load_entries_inner(&shared).unwrap()
    }

    #[test]
    fn counts_metrics_rows_and_deduplicates_streaming_snapshots() {
        let fixture = fs_fixture!({});
        let _ = fixture.create_dir_all("cli");
        let db_path = fixture.path("cli/sessions.db");
        create_db(&db_path);
        let db = sqlite::open(&db_path).unwrap();
        insert_session(&db, "alpha-session", "/project/alpha", "swe-2-max");
        let message = assistant_message(
            "req-1",
            Some("swe-2-max"),
            r#""input_tokens":7386,"output_tokens":324,"cache_read_tokens":10496,"cache_creation_tokens":null,"context_input_tokens":17882"#,
        );
        // Streaming commits of the same request must count once.
        insert_node(&db, "alpha-session", 1, &message, 1793612320);
        insert_node(&db, "alpha-session", 1, &message, 1793612328);
        insert_node(
            &db,
            "alpha-session",
            2,
            &assistant_message("req-2", None, r#""input_tokens":100,"output_tokens":10"#),
            1793612400,
        );
        drop(db);

        let entries = load(fixture.root());

        assert_eq!(entries.len(), 2);
        let first = &entries[0];
        assert_eq!(
            first.data.message.id.as_deref(),
            Some("devin:alpha-session:req-1")
        );
        assert_eq!(first.data.message.usage.input_tokens, 7386);
        assert_eq!(first.data.message.usage.output_tokens, 324);
        assert_eq!(first.data.message.usage.cache_read_input_tokens, 10496);
        assert_eq!(first.data.message.usage.cache_creation_input_tokens, 0);
        assert_eq!(first.timestamp.as_millis(), 1793612328000);
        assert_eq!(first.project_path.as_ref(), "/project/alpha");
        assert_eq!(first.model.as_deref(), Some("swe-2-max"));
        assert_eq!(first.cost, 0.0);
        // generation_model missing -> falls back to the session's model column.
        assert_eq!(entries[1].model.as_deref(), Some("swe-2-max"));
    }

    #[test]
    fn skips_rows_without_metrics_or_non_assistant_roles() {
        let fixture = fs_fixture!({});
        let _ = fixture.create_dir_all("cli");
        let db_path = fixture.path("cli/sessions.db");
        create_db(&db_path);
        let db = sqlite::open(&db_path).unwrap();
        insert_node(
            &db,
            "alpha-session",
            1,
            r#"{"message_id":"u1","role":"user","content":"hi","metadata":{}}"#,
            1793612300,
        );
        insert_node(
            &db,
            "alpha-session",
            2,
            r#"{"message_id":"a1","role":"assistant","content":"","metadata":{"request_id":"r-empty"}}"#,
            1793612310,
        );
        insert_node(
            &db,
            "alpha-session",
            3,
            &assistant_message("r-zero", None, r#""input_tokens":0,"output_tokens":0"#),
            1793612320,
        );
        drop(db);

        assert!(load(fixture.root()).is_empty());
    }

    #[test]
    fn reads_wal_database_and_combines_multiple_homes() {
        let first = fs_fixture!({});
        let _ = first.create_dir_all("cli");
        let first_db = first.path("cli/sessions.db");
        create_db(&first_db);
        let db = sqlite::open(&first_db).unwrap();
        db.execute("PRAGMA journal_mode = WAL").unwrap();
        insert_session(&db, "s1", "/p1", "swe-2-high");
        insert_node(
            &db,
            "s1",
            1,
            &assistant_message("req-1", None, r#""input_tokens":10,"output_tokens":5"#),
            1793612300,
        );
        drop(db);

        let second = fs_fixture!({});
        let _ = second.create_dir_all("cli");
        let second_db = second.path("cli/cli_sessions.db");
        create_db(&second_db);
        let db = sqlite::open(&second_db).unwrap();
        insert_session(&db, "s2", "/p2", "swe-2-max");
        insert_node(
            &db,
            "s2",
            1,
            &assistant_message("req-2", None, r#""input_tokens":20,"output_tokens":7"#),
            1793612400,
        );
        drop(db);

        let _guard = EnvVarGuard::set(
            DEVIN_HOME_ENV,
            format!("{},{}", first.root().display(), second.root().display()),
        );
        let shared = SharedArgs {
            timezone: Some("UTC".to_string()),
            ..SharedArgs::default()
        };
        let entries = load_entries_inner(&shared).unwrap();

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].session_id.as_ref(), "s1");
        assert_eq!(entries[1].session_id.as_ref(), "s2");
    }

    #[test]
    fn ignores_unrelated_schema() {
        let fixture = fs_fixture!({});
        let _ = fixture.create_dir_all("cli");
        let db_path = fixture.path("cli/sessions.db");
        let db = sqlite::open(&db_path).unwrap();
        db.execute("CREATE TABLE unrelated (id TEXT)").unwrap();
        drop(db);

        assert!(load(fixture.root()).is_empty());
    }
}
