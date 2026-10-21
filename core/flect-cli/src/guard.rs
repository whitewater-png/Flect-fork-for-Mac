//! Safety rails for the tasks that delete things (`folder.removeDuplicate`, `folder.removeEmpty`).
//!
//! A command line has no confirmation dialog, so the CLI never deletes by default: these tasks
//! are forced into their preview form (`dry=1` for duplicates; no `confirm=1` for empty folders).
//! Deleting needs `--apply`, and when the saved setting is "delete permanently" it additionally
//! needs `--permanent`. Any `dry` / `confirm` the user passed with `-o` is ignored.
//!
//! The decision is made on what the engine will read: every option name is percent-decoded and
//! compared without regard to case, so `%63onfirm`, `CONFIRM` or a repeated `dry` are dropped too,
//! and a name that is not a plain `[A-Za-z][A-Za-z0-9_]*` once decoded (`+confirm`, ` confirm`,
//! `confirm%00`...) is refused outright. (The engine itself refuses encoded, malformed or repeated
//! names, so these could not take effect either way.)
use flect_core::tasks::{option_name_ok, percent_decode};

/// `dry` / `confirm` in any spelling the engine or a human could take for them.
fn is_guarded(decoded: &str) -> bool {
    decoded.eq_ignore_ascii_case("dry") || decoded.eq_ignore_ascii_case("confirm")
}

/// Rewrite `task` (`ID?k=v&...`) according to the rules above. `setting_permanent` is the user's
/// saved delete method. Returns the task to send, or a usage error.
pub fn guard_task(
    task: &str,
    apply: bool,
    permanent: bool,
    setting_permanent: bool,
) -> Result<String, String> {
    let (name, query) = task.split_once('?').unwrap_or((task, ""));
    let duplicates = name == "folder.removeDuplicate";
    if !duplicates && name != "folder.removeEmpty" {
        if apply || permanent {
            return Err(format!("--apply / --permanent do not apply to {name}"));
        }
        return Ok(task.to_owned());
    }
    if permanent && !apply {
        return Err("--permanent only makes sense together with --apply".into());
    }
    if apply && setting_permanent && !permanent {
        return Err(
            "your setting deletes permanently (no Trash): add --permanent as well as --apply to confirm"
                .into(),
        );
    }
    let mut parts: Vec<String> = Vec::new();
    for p in query.split('&').filter(|p| !p.is_empty()) {
        let raw = p.split_once('=').map_or(p, |(k, _)| k);
        let decoded = percent_decode(raw);
        if is_guarded(&decoded) {
            continue;
        }
        if !option_name_ok(&decoded) || decoded != raw {
            return Err(format!("invalid option name in {name}: {raw:?}"));
        }
        parts.push(p.to_owned());
    }
    match (duplicates, apply) {
        (true, false) => parts.push("dry=1".into()),
        (false, true) => parts.push("confirm=1".into()),
        _ => {}
    }
    Ok(if parts.is_empty() {
        name.to_owned()
    } else {
        format!("{name}?{}", parts.join("&"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_preview_and_user_overrides_are_dropped() {
        let g = |t, a, p, s| guard_task(t, a, p, s);
        assert_eq!(
            g("folder.removeDuplicate?mode=all", false, false, true).unwrap(),
            "folder.removeDuplicate?mode=all&dry=1"
        );
        assert_eq!(
            g("folder.removeDuplicate?dry=0&mode=all", false, false, true).unwrap(),
            "folder.removeDuplicate?mode=all&dry=1"
        );
        assert_eq!(
            g("folder.removeEmpty?confirm=1", false, false, true).unwrap(),
            "folder.removeEmpty"
        );
    }

    #[test]
    fn apply_needs_permanent_when_the_setting_is_permanent() {
        assert!(guard_task("folder.removeEmpty", true, false, true).is_err());
        assert_eq!(
            guard_task("folder.removeEmpty", true, true, true).unwrap(),
            "folder.removeEmpty?confirm=1"
        );
        assert_eq!(
            guard_task("folder.removeEmpty", true, false, false).unwrap(),
            "folder.removeEmpty?confirm=1"
        );
        assert_eq!(
            guard_task("folder.removeDuplicate?dry=1&mode=newest", true, true, true).unwrap(),
            "folder.removeDuplicate?mode=newest"
        );
        assert!(guard_task("folder.removeEmpty", false, true, false).is_err());
    }

    /// `%63onfirm=1` used to pass the guard (raw-name match) and be decoded to `confirm` by the engine.
    #[test]
    fn encoded_case_and_repeated_guard_names_are_dropped_or_refused() {
        for (task, want) in [
            ("folder.removeEmpty?%63onfirm=1", "folder.removeEmpty"),
            (
                "folder.removeEmpty?%63%6f%6e%66%69%72%6d=%31",
                "folder.removeEmpty",
            ),
            ("folder.removeEmpty?%43onfirm=1", "folder.removeEmpty"),
            (
                "folder.removeEmpty?Confirm=1&CONFIRM=1",
                "folder.removeEmpty",
            ),
            (
                "folder.removeEmpty?confirm=1&confirm=1",
                "folder.removeEmpty",
            ),
            ("folder.removeEmpty?confirm", "folder.removeEmpty"),
            (
                "folder.removeDuplicate?mode=all&%64ry=0",
                "folder.removeDuplicate?mode=all&dry=1",
            ),
            (
                "folder.removeDuplicate?dry=1&dry=0&mode=all",
                "folder.removeDuplicate?mode=all&dry=1",
            ),
            (
                "folder.removeDuplicate?mode=all&DRY=0&%44Ry=0",
                "folder.removeDuplicate?mode=all&dry=1",
            ),
        ] {
            assert_eq!(
                guard_task(task, false, false, true).unwrap(),
                want,
                "{task}"
            );
        }
        for task in [
            "folder.removeEmpty?+confirm=1",
            "folder.removeEmpty?%20confirm=1",
            "folder.removeEmpty? confirm=1",
            "folder.removeEmpty?confirm%00=1",
            "folder.removeEmpty?confirm%3D1",
            "folder.removeDuplicate?mode=all&+dry=0",
            "folder.removeDuplicate?%6dode=all",
            "folder.removeDuplicate?1x=0",
        ] {
            assert!(guard_task(task, false, false, true).is_err(), "{task}");
            assert!(guard_task(task, true, true, true).is_err(), "{task}");
        }
        // with --apply the user's own spellings are dropped and exactly one is added
        assert_eq!(
            guard_task("folder.removeEmpty?%63onfirm=0", true, true, true).unwrap(),
            "folder.removeEmpty?confirm=1"
        );
        assert_eq!(
            guard_task("folder.removeDuplicate?mode=all&%64ry=1", true, true, true).unwrap(),
            "folder.removeDuplicate?mode=all"
        );
    }

    #[test]
    fn other_tasks_are_untouched_and_reject_the_flags() {
        assert_eq!(
            guard_task("image.resize?scale=50", false, false, true).unwrap(),
            "image.resize?scale=50"
        );
        assert!(guard_task("image.resize", true, false, false).is_err());
    }
}
