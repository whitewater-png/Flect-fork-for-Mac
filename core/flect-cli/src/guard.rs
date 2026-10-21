//! Safety rails for the tasks that delete things (`folder.removeDuplicate`, `folder.removeEmpty`).
//!
//! A command line has no confirmation dialog, so the CLI never deletes by default: these tasks
//! are forced into their preview form (`dry=1` for duplicates; no `confirm=1` for empty folders).
//! Deleting needs `--apply`, and when the saved setting is "delete permanently" it additionally
//! needs `--permanent`. Any `dry` / `confirm` the user passed with `-o` is ignored.

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
    let mut parts: Vec<String> = query
        .split('&')
        .filter(|p| !p.is_empty())
        .filter(|p| !matches!(p.split('=').next(), Some("dry" | "confirm")))
        .map(str::to_owned)
        .collect();
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

    #[test]
    fn other_tasks_are_untouched_and_reject_the_flags() {
        assert_eq!(
            guard_task("image.resize?scale=50", false, false, true).unwrap(),
            "image.resize?scale=50"
        );
        assert!(guard_task("image.resize", true, false, false).is_err());
    }
}
