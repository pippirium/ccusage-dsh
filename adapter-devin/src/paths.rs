use std::{collections::HashSet, env, fs, path::PathBuf};

use crate::{cli::SharedArgs, debug_log};

pub(crate) const DEVIN_HOME_ENV: &str = "DEVIN_HOME";
pub(crate) const DEVIN_SESSIONS_DIR_ENV: &str = "DEVIN_SESSIONS_DIR";
pub(crate) const DB_FILE_NAMES: &[&str] = &["sessions.db", "cli_sessions.db"];
const CLI_DIR: &str = "cli";

/// Directories that may hold Devin's session store. `DEVIN_HOME` roots take a
/// `cli/` subdirectory, matching the DSH_HOME/ZCODE_HOME convention; Devin's own
/// `DEVIN_SESSIONS_DIR` points at the directory containing the database itself.
fn candidate_dirs() -> Vec<PathBuf> {
    if let Some(raw) = env::var_os(DEVIN_HOME_ENV) {
        let roots: Vec<PathBuf> = match raw.into_string() {
            Ok(value) => value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|root| PathBuf::from(root).join(CLI_DIR))
                .collect(),
            Err(raw) => vec![PathBuf::from(raw).join(CLI_DIR)],
        };
        if !roots.is_empty() {
            return roots;
        }
    }
    if let Some(dir) = env::var_os(DEVIN_SESSIONS_DIR_ENV) {
        return vec![PathBuf::from(dir)];
    }
    let mut dirs = Vec::new();
    if let Some(xdg) = env::var_os("XDG_DATA_HOME") {
        dirs.push(PathBuf::from(xdg).join("devin").join(CLI_DIR));
    }
    if let Some(home) = crate::home::home_dir() {
        dirs.push(
            home.join(".local")
                .join("share")
                .join("devin")
                .join(CLI_DIR),
        );
        // Pre-rename installs kept history under the cognition directory; newer
        // releases leave a symlink there, so canonicalization dedupes it.
        dirs.push(
            home.join(".local")
                .join("share")
                .join("cognition")
                .join(CLI_DIR),
        );
    }
    dirs
}

pub(crate) fn db_paths(shared: &SharedArgs) -> Vec<PathBuf> {
    let mut db_paths = Vec::new();
    let mut seen = HashSet::new();
    for dir in candidate_dirs() {
        for name in DB_FILE_NAMES {
            let candidate = dir.join(name);
            let canonical = match fs::canonicalize(&candidate) {
                Ok(path) if path.is_file() => path,
                _ => continue,
            };
            if seen.insert(canonical.clone()) {
                db_paths.push(canonical);
            }
            // A session directory holds one store; the legacy name is only a
            // fallback for unmigrated installs.
            break;
        }
        if db_paths.is_empty() && !dir.exists() {
            debug_log(
                shared,
                format!("Devin data directory does not exist: {}", dir.display()),
            );
        }
    }
    db_paths
}
