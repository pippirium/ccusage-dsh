# ccusage-adapter-devin

[Devin CLI](https://devin.ai) usage adapter for ccusage. Token counts are reported
as recorded; cost is always **$0** because Devin bills through ACUs/credits on the
server side and local data carries no USD pricing.

## Data source

The adapter reads the CLI's session history SQLite store:

- `$DEVIN_SESSIONS_DIR/sessions.db` when `DEVIN_SESSIONS_DIR` is set (Devin's own
  override for the directory holding the store).
- Otherwise `cli/sessions.db` under each `DEVIN_HOME` root (comma-separated, same
  convention as `DSH_HOME`/`ZCODE_HOME`), falling back to
  `$XDG_DATA_HOME/devin/cli/`, `~/.local/share/devin/cli/`, and the pre-rename
  `~/.local/share/cognition/cli/`. The legacy `cli_sessions.db` name is accepted
  alongside `sessions.db` in each directory.

The database is opened read-only with `query_only` and a busy timeout, so a
running Devin CLI (WAL journal) is not disturbed.

Devin's experimental `session_store = files` backend is not read; sessions that
were already imported out of `sessions.db` are simply absent.

## Token mapping

One row per settled request in `message_nodes` (`role = 'assistant'` rows whose
`metadata.metrics` object exists). Streaming snapshots of the same message are
deduplicated by `metadata.request_id`, keeping the last committed row.

- `input_tokens` -> `input_tokens` (uncached input; disjoint from cache reads)
- `cache_read_tokens` -> `cache_read_input_tokens`
- `cache_creation_tokens` -> `cache_creation_input_tokens` (currently always null)
- `output_tokens` -> `output_tokens`
- `context_input_tokens` is a derived prompt-size figure (input + cache read),
  not an additional category, so it is not counted again.

Model comes from `metadata.generation_model`, falling back to the session's
`model` column; `project_path` is the session `working_directory`. Timestamps use
the node `created_at` epoch seconds.
