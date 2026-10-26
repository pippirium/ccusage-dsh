use ccusage_core::cli::SharedArgs;
use ccusage_core::*;
use serde::Deserialize;
use std::{
    collections::{BTreeMap, HashSet},
    env,
    fs::{self, File},
    io::{self, BufRead, BufReader, Read},
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Usage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_tokens: u64,
    #[serde(default, alias = "cacheCreationTokens")]
    cache_write_tokens: u64,
    #[serde(default)]
    total_tokens: u64,
}
#[derive(Default, Deserialize)]
struct Config {
    model: Option<String>,
}
#[derive(Default, Deserialize)]
struct Header {
    #[serde(default)]
    config: Config,
}
#[derive(Default, Deserialize)]
struct Chunk {
    usage: Option<Usage>,
}
#[derive(Default, Deserialize)]
struct StreamRecord {
    chunk: Option<Chunk>,
}
#[derive(Default, Deserialize)]
struct Data {
    turn: Option<u64>,
    step: Option<u64>,
    usage: Option<Usage>,
    #[serde(default)]
    stream: Vec<StreamRecord>,
    header: Option<Header>,
}
#[derive(Deserialize)]
struct Record {
    #[serde(rename = "type")]
    kind: String,
    version: Option<u64>,
    #[serde(default, rename = "isSeeded")]
    is_seeded: bool,
    id: Option<String>,
    cwd: Option<String>,
    seq: Option<u64>,
    time: Option<i64>,
    #[serde(default)]
    data: Data,
}

fn root() -> Option<PathBuf> {
    env::var_os("DSH_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|p| PathBuf::from(p).join(".dsh")))
        .map(|p| p.join("sessions"))
}

// Prefer the latest supported log in a session directory after a v3 -> v4 migration.
fn discover(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut directories = vec![root.to_owned()];
    let mut files = Vec::new();
    while let Some(dir) = directories.pop() {
        let children = match fs::read_dir(&dir) {
            Ok(children) => children,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        let mut selected: Option<(u64, PathBuf)> = None;
        for child in children {
            let child = child?;
            let kind = child.file_type()?;
            if kind.is_dir() {
                directories.push(child.path());
            }
            if !kind.is_file() {
                continue;
            }
            let name = child.file_name();
            let name = name.to_string_lossy();
            let version = name
                .strip_prefix("session.v")
                .and_then(|s| s.strip_suffix(".jsonl.zstd"))
                .and_then(|s| s.parse::<u64>().ok());
            if let Some(v) = version {
                if !(3..=4).contains(&v) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "Unsupported DSH log version {v}: {}",
                            child.path().display()
                        ),
                    ));
                }
                if selected.as_ref().is_none_or(|(old, _)| v > *old) {
                    selected = Some((v, child.path()));
                }
            }
        }
        if let Some((_, path)) = selected {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

pub(crate) fn has_data() -> bool {
    let Some(root) = root() else {
        return false;
    };
    let mut directories = vec![root];
    while let Some(dir) = directories.pop() {
        let Ok(children) = fs::read_dir(dir) else {
            continue;
        };
        for child in children.flatten() {
            let Ok(kind) = child.file_type() else {
                continue;
            };
            if kind.is_dir() {
                directories.push(child.path());
            }
            if kind.is_file()
                && matches!(
                    child.file_name().to_str(),
                    Some("session.v3.jsonl.zstd" | "session.v4.jsonl.zstd")
                )
            {
                return true;
            }
        }
    }
    false
}

pub fn load_entries(shared: &SharedArgs) -> Result<Vec<LoadedEntry>> {
    let Some(root) = root() else {
        return Ok(Vec::new());
    };
    load_from_root(&root, shared)
}

fn load_from_root(root: &Path, shared: &SharedArgs) -> Result<Vec<LoadedEntry>> {
    let mut entries = BTreeMap::new();
    for path in discover(root)? {
        let decoder = zstd::stream::read::Decoder::new(File::open(&path)?)?;
        for entry in read_log(decoder, shared, &path)? {
            let key = (entry.session_id.to_string(), entry.data.message.id.clone());
            if let Some(previous) = entries.get(&key) {
                let previous: &LoadedEntry = previous;
                if signature(previous) != signature(&entry) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("Conflicting DSH event copies in {}", path.display()),
                    )
                    .into());
                }
            } else {
                entries.insert(key, entry);
            }
        }
    }
    let mut entries: Vec<_> = entries.into_values().collect();
    entries.sort_by_key(|entry| entry.timestamp);
    Ok(entries)
}

fn read_log(reader: impl Read, shared: &SharedArgs, path: &Path) -> Result<Vec<LoadedEntry>> {
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    let mut session = None;
    let mut project = String::new();
    let mut model = None;
    let mut in_seed = false;
    let mut entries = Vec::new();
    let mut seqs = HashSet::new();
    let mut last_slot: Option<(u64, u64, usize)> = None;
    let tz = parse_tz(shared.timezone.as_deref());
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                eprintln!(
                    "DSH: incomplete compressed tail in {}; counting complete records only",
                    path.display()
                );
                break;
            }
            Err(e) => return Err(e.into()),
        }
        if !line.ends_with('\n') {
            eprintln!(
                "DSH: incomplete JSONL tail in {}; counting complete records only",
                path.display()
            );
            break;
        }
        let record: Record = serde_json::from_str(&line).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Invalid DSH record in {}: {e}", path.display()),
            )
        })?;
        if record.kind == "session" {
            if !matches!(record.version, Some(3 | 4)) || session.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Unsupported or duplicate DSH session header",
                )
                .into());
            }
            in_seed = record.is_seeded;
            session = record.id;
            project = record.cwd.unwrap_or_else(|| "unknown".to_owned());
            continue;
        }
        if record.kind == "request/header" {
            model = record.data.header.and_then(|h| h.config.model);
            continue;
        }
        if record.kind == "session/end-seed" {
            in_seed = false;
            continue;
        }
        if in_seed {
            continue;
        }
        if record.kind == "llm/retry-started" {
            if last_slot.is_some_and(|(turn, step, _)| {
                record.data.turn == Some(turn) && record.data.step == Some(step)
            }) {
                last_slot = None;
            }
            continue;
        }
        if !matches!(
            record.kind.as_str(),
            "assistant/message" | "assistant/attempt"
        ) {
            continue;
        }
        // Settled-message usage is authoritative. An uncommitted attempt only has
        // stream usage; take its final accounting snapshot, never sum snapshots.
        let authoritative = if record.kind == "assistant/message" {
            record.data.usage
        } else {
            None
        };
        let usage = authoritative.or_else(|| {
            record
                .data
                .stream
                .into_iter()
                .filter_map(|r| r.chunk.and_then(|c| c.usage))
                .last()
        });
        let Some(usage) = usage else {
            continue;
        };
        let session = session.as_ref().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "DSH usage without session id")
        })?;
        let seq = record.seq.ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "DSH usage without event seq")
        })?;
        if !seqs.insert(seq) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Duplicate DSH usage event seq",
            )
            .into());
        }
        let timestamp = TimestampMs::from_millis(record.time.ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "DSH usage without timestamp")
        })?);
        let tokens = TokenUsageRaw {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_creation_input_tokens: usage.cache_write_tokens,
            cache_read_input_tokens: usage.cache_read_tokens,
            speed: None,
            cache_creation: None,
        };
        // DSH normalizes inputTokens to uncached input. Cache tokens are disjoint.
        // Preserve any provider-reported total not represented by these parts.
        let extra_total_tokens = usage
            .total_tokens
            .saturating_sub(total_usage_tokens(tokens));
        let model = model.clone().unwrap_or_else(|| "dsh-unknown".to_owned());
        let entry = LoadedEntry {
            data: UsageEntry {
                session_id: Some(session.clone()),
                timestamp: format_rfc3339_millis(timestamp),
                version: None,
                message: UsageMessage {
                    usage: tokens,
                    model: Some(model.clone()),
                    id: Some(format!("dsh:{session}:{seq}")),
                },
                cost_usd: Some(0.0),
                request_id: None,
                is_api_error_message: None,
                is_sidechain: None,
            },
            timestamp,
            date: format_date_tz(timestamp, tz.as_ref()),
            project: Arc::from("dsh"),
            session_id: Arc::from(session.as_str()),
            project_path: Arc::from(project.as_str()),
            cost: 0.0,
            credits: None,
            model: Some(model),
            message_count: Some(1),
            usage_limit_reset_time: None,
            missing_pricing_model: None,
            extra_total_tokens,
            stored_cost: Some(0.0),
        };
        // Match DSH token-meter: a settlement updates the current turn/step
        // slot until llm/retry-started closes it. A retry is a new request.
        let slot = record.data.turn.zip(record.data.step);
        if let Some((turn, step, index)) =
            last_slot.filter(|(turn, step, _)| slot == Some((*turn, *step)))
        {
            entries[index] = entry;
            last_slot = Some((turn, step, index));
        } else {
            let index = entries.len();
            entries.push(entry);
            last_slot = slot.map(|(turn, step)| (turn, step, index));
        }
    }
    entries.retain(|entry| {
        total_usage_tokens(entry.data.message.usage) > 0 || entry.extra_total_tokens > 0
    });
    Ok(entries)
}

#[cfg(test)]
mod tests;

fn signature(entry: &LoadedEntry) -> (i64, &str, &str, u64, u64, u64, u64, u64) {
    let u = entry.data.message.usage;
    (
        entry.timestamp.as_millis(),
        entry.project_path.as_ref(),
        entry.model.as_deref().unwrap_or_default(),
        u.input_tokens,
        u.output_tokens,
        u.cache_read_input_tokens,
        u.cache_creation_input_tokens,
        entry.extra_total_tokens,
    )
}
