//! Where Flect keeps its files. `FLECT_HOME` overrides everything (used by tests).
use std::path::PathBuf;

pub const APP_DIR: &str = "Flect-for-Mac";
const HOME_VARIABLE: &str = "FLECT_HOME";

/// Directory for settings and caches.
pub fn data_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os(HOME_VARIABLE).filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home));
    }
    let home = std::env::var_os("HOME").filter(|v| !v.is_empty())?;
    Some(
        PathBuf::from(home)
            .join("Library/Application Support")
            .join(APP_DIR),
    )
}
