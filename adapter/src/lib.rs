use ccusage_adapter_common::filter_loaded_entries_by_date;
use ccusage_core::*;

mod loader;

mod report;

use crate::{
    Result,
    cli::{AgentCommandArgs, AgentReportKind, SharedArgs},
    print_json_or_jq, print_usage_table, sort_summaries, wants_json,
};

pub use loader::load_entries;
pub(crate) use report::report_from_rows;
pub use report::summarize_entries;

pub fn run(args: AgentCommandArgs) -> Result<()> {
    let mut entries = load_entries(&args.shared)?;
    let mut rows = if args.kind == AgentReportKind::Session {
        summarize_entries(&entries, args.kind)?
    } else {
        filter_loaded_entries_by_date(&mut entries, &args.shared);
        summarize_entries(&entries, args.kind)?
    };
    if args.kind == AgentReportKind::Session {
        filter_session_summaries(&mut rows, &args.shared);
    }
    sort_summaries(&mut rows, &args.shared.order, |row| {
        ccusage_core::summary_period(row)
    });
    if wants_json(&args.shared) {
        return print_json_or_jq(
            report_from_rows(&rows, args.kind),
            args.shared.jq.as_deref(),
            args.shared.no_cost,
        );
    }
    print_usage_table(
        "DSH Token Usage Report",
        ccusage_core::first_column(args.kind),
        &rows,
        &args.shared,
        false,
        None,
    )?;
    Ok(())
}

fn filter_session_summaries(rows: &mut Vec<crate::UsageSummary>, shared: &SharedArgs) {
    if shared.since.is_some() || shared.until.is_some() {
        let since = shared.since.as_deref().map(|value| value.replace('-', ""));
        let until = shared.until.as_deref().map(|value| value.replace('-', ""));
        rows.retain(|row| {
            let date = row
                .last_activity
                .as_deref()
                .unwrap_or_default()
                .replace('-', "");
            since.as_ref().is_none_or(|bound| &date >= bound)
                && until.as_ref().is_none_or(|bound| &date <= bound)
        });
    }
}

pub fn has_data() -> bool {
    loader::has_data()
}
