//! Tasks that look at the whole selection at once (the folder family). Results and requests for
//! the UI go out as protocol events through `emit`; nothing here prompts the user.
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::json;

use crate::folder::{self, DuplicateMode, Progress};
use crate::foldertasks::{self as ft, RenamePlan};
use crate::protocol::{Event, Outcome};
use crate::settings::{DeleteMethod, DuplicateKeep, Settings};
use crate::tasks::{self, parse_task};

pub fn is_batch_task(name: &str) -> bool {
    matches!(
        name,
        "folder.removeDuplicate"
            | "folder.removeEmpty"
            | "folder.tree"
            | "folder.rename"
            | "folder.analyze"
    )
}

pub struct Ctx<'a> {
    pub settings: &'a Settings,
    pub cancelled: &'a (dyn Fn() -> bool + Sync),
    pub emit: &'a (dyn Fn(Event) + Sync),
}

struct Prog<'a>(&'a Ctx<'a>);

impl Progress for Prog<'_> {
    fn cancelled(&self) -> bool {
        (self.0.cancelled)()
    }
    fn status(&self, key: &str, args: &[String]) {
        (self.0.emit)(Event::Status {
            key: key.into(),
            args: args.to_vec(),
        });
    }
    fn step(&self, done: u64, total: u64) {
        (self.0.emit)(Event::Step { done, total });
    }
}

fn done(ctx: &Ctx<'_>, outcome: Outcome, key: Option<&str>, args: Vec<String>) {
    (ctx.emit)(Event::Done {
        outcome,
        summary_key: key.map(str::to_owned),
        summary_args: args,
    });
}

fn cancelled(ctx: &Ctx<'_>) {
    done(ctx, Outcome::Cancelled, None, vec![]);
}

fn fail(ctx: &Ctx<'_>, key: &str, arg: String) {
    (ctx.emit)(Event::Error {
        key: key.into(),
        args: vec![arg],
    });
    done(ctx, Outcome::Failed, None, vec![]);
}

/// Selected items that are folders (symlinks are not followed), each once: the same folder
/// selected twice (or under two spellings, such as `/var/x` and `/private/var/x`) counts once.
/// Folders inside another selected folder are kept here (rename works per folder) and collapsed by
/// the recursive scans themselves (`folder::scan`).
fn folders(paths: &[String]) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    paths
        .iter()
        .map(PathBuf::from)
        .filter(|p| p.symlink_metadata().map(|m| m.is_dir()).unwrap_or(false))
        .filter(|p| seen.insert(std::fs::canonicalize(p).unwrap_or_else(|_| p.clone())))
        .collect()
}

/// The selected items (files and folders; for a link both the link and its target) by (device,
/// inode), and the same plus every folder that holds one of them (along the spelled and the
/// resolved path). Neither deleting task ever removes a selected item,
/// or a folder that holds one, in either Trash or permanent mode.
type IdSet = std::collections::HashSet<(u64, u64)>;

fn selected_ids(paths: &[String]) -> (IdSet, IdSet) {
    use std::os::unix::fs::MetadataExt;
    let (mut items, mut holders) = (
        std::collections::HashSet::new(),
        std::collections::HashSet::new(),
    );
    for p in paths {
        let p = Path::new(p);
        if let Ok(m) = std::fs::symlink_metadata(p) {
            items.insert((m.dev(), m.ino()));
        }
        // A selected link stands for what it points to as well: both are protected.
        if let Ok(m) = std::fs::metadata(p) {
            items.insert((m.dev(), m.ino()));
        }
        for spelled in [Some(p.to_path_buf()), std::fs::canonicalize(p).ok()]
            .into_iter()
            .flatten()
        {
            for a in spelled.ancestors().skip(1) {
                if let Some(id) = folder::dir_id(a) {
                    holders.insert(id);
                }
            }
        }
    }
    holders.extend(items.iter().copied());
    (items, holders)
}

fn path_list(v: &[PathBuf]) -> Vec<String> {
    v.iter().map(|p| p.to_string_lossy().into_owned()).collect()
}

fn mode_from(
    params: &std::collections::HashMap<String, String>,
    keep: DuplicateKeep,
) -> Option<DuplicateMode> {
    match params.get("mode").map(String::as_str) {
        Some("oldest") => Some(DuplicateMode::KeepOldest),
        Some("newest") => Some(DuplicateMode::KeepNewest),
        Some("all") => Some(DuplicateMode::DeleteAll),
        Some(_) => None,
        None => match keep {
            DuplicateKeep::Oldest => Some(DuplicateMode::KeepOldest),
            DuplicateKeep::Newest => Some(DuplicateMode::KeepNewest),
            DuplicateKeep::Ask => None,
        },
    }
}

fn file_json(f: &folder::ScannedFile) -> serde_json::Value {
    json!({
        "path": f.path.to_string_lossy(),
        "size": f.size,
        "modified": f.modified.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
    })
}

/// The dry-run view of a duplicate removal: per group the copy that stays (none for "delete all") and
/// the copies that go, built from the same `doomed` list that a real run would hand on.
fn duplicate_preview(
    groups: &[Vec<folder::ScannedFile>],
    doomed: &[folder::ScannedFile],
    mode: DuplicateMode,
    method: DeleteMethod,
    bytes: u64,
) -> serde_json::Value {
    let gone: std::collections::HashSet<&Path> = doomed.iter().map(|d| d.path.as_path()).collect();
    let listing: Vec<_> = groups
        .iter()
        .map(|g| {
            let keep = g.iter().find(|f| !gone.contains(f.path.as_path()));
            let remove: Vec<_> = g
                .iter()
                .filter(|f| gone.contains(f.path.as_path()))
                .map(file_json)
                .collect();
            json!({"keep": keep.map(file_json), "remove": remove})
        })
        .collect();
    json!({
        "mode": match mode {
            DuplicateMode::KeepOldest => "oldest",
            DuplicateMode::KeepNewest => "newest",
            DuplicateMode::DeleteAll => "all",
        },
        "groups": listing,
        "removeCount": doomed.len(),
        "reclaimBytes": bytes,
        "permanent": method == DeleteMethod::Permanent,
    })
}

/// A duplicate that is not removed because it, or the copy that stays, is not what the scan saw.
fn changed_event(ctx: &Ctx<'_>, p: &Path) {
    (ctx.emit)(Event::Warn {
        key: "msg.duplicateChanged".into(),
        args: vec![p.to_string_lossy().into_owned()],
    });
}

/// [`folder::plan_still_holds`], reporting what changed when it does not hold: files keep changing
/// while the plan is made (an app saving, a sync client), so this is a normal outcome, not an error.
/// Nothing is then shown or removed.
fn plan_holds_or_report(
    ctx: &Ctx<'_>,
    groups: &[Vec<folder::ScannedFile>],
    doomed: &[folder::ScannedFile],
) -> bool {
    if folder::plan_still_holds(groups, doomed) {
        return true;
    }
    let (_, mut changed) = folder::unchanged_groups(groups.to_vec());
    if changed.is_empty() {
        changed = doomed.iter().map(|d| d.path.clone()).collect();
    }
    for p in &changed {
        changed_event(ctx, p);
    }
    false
}

/// Carry out a confirmed duplicate plan, one file at a time. Right before each file is removed
/// (permanent) or handed on to the app for the Trash, it and the copy that stays are checked again
/// and the removal goes through handles of the verified folder ([`folder::remove_duplicate`]); a
/// file that fails is skipped and reported with `msg.duplicateChanged`. The summary counts what was
/// removed.
fn remove_duplicates(
    ctx: &Ctx<'_>,
    groups: &[Vec<folder::ScannedFile>],
    doomed: &[folder::ScannedFile],
    mode: DuplicateMode,
    roots: &[PathBuf],
) {
    remove_duplicates_with(ctx, groups, doomed, mode, roots, &|_| {});
}

/// For each copy that goes, the copy that stays in its group (none in "delete all" mode), built
/// in one pass over the groups (a file -> group map), not by searching the groups for every file.
fn kept_copies<'a>(
    groups: &'a [Vec<folder::ScannedFile>],
    doomed: &[folder::ScannedFile],
    mode: DuplicateMode,
) -> std::collections::HashMap<&'a Path, &'a folder::ScannedFile> {
    let mut out = std::collections::HashMap::new();
    if mode == DuplicateMode::DeleteAll {
        return out;
    }
    let gone: std::collections::HashSet<&Path> = doomed.iter().map(|d| d.path.as_path()).collect();
    for g in groups {
        if let Some(kept) = g.iter().find(|f| !gone.contains(f.path.as_path())) {
            for f in g.iter().filter(|f| gone.contains(f.path.as_path())) {
                out.insert(f.path.as_path(), kept);
            }
        }
    }
    out
}

/// [`remove_duplicates`] with a hook that runs after a file's checks, right before it is removed.
fn remove_duplicates_with(
    ctx: &Ctx<'_>,
    groups: &[Vec<folder::ScannedFile>],
    doomed: &[folder::ScannedFile],
    mode: DuplicateMode,
    roots: &[PathBuf],
    before_unlink: &dyn Fn(&folder::ScannedFile),
) {
    let permanent = ctx.settings.folder.delete_method == DeleteMethod::Permanent;
    let kept_of = kept_copies(groups, doomed, mode);
    let kept_of = |d: &folder::ScannedFile| kept_of.get(d.path.as_path()).copied();
    let prog = Prog(ctx);
    let (mut handed, mut bytes, mut skipped) = (Vec::new(), 0u64, 0usize);
    // Cancelled part-way: say how many were already removed (permanent) or handed on (Trash).
    let stopped = |handed: &Vec<PathBuf>, bytes: u64| {
        if handed.is_empty() {
            return cancelled(ctx);
        }
        let key = if permanent {
            "msg.cancelledAfterDeleting"
        } else {
            "msg.cancelledAfterTrashing"
        };
        done(
            ctx,
            Outcome::Cancelled,
            Some(key),
            vec![handed.len().to_string(), bytes.to_string()],
        );
    };
    for (i, d) in doomed.iter().enumerate() {
        if prog.cancelled() {
            return stopped(&handed, bytes);
        }
        let kept = kept_of(d);
        if (mode != DuplicateMode::DeleteAll && kept.is_none()) || !roots.contains(&d.root.path) {
            changed_event(ctx, &d.path);
            skipped += 1;
            continue;
        }
        match folder::remove_duplicate(d, kept, permanent, &prog, before_unlink) {
            Err(_) => return stopped(&handed, bytes),
            Ok(folder::Removal::Changed) => {
                changed_event(ctx, &d.path);
                skipped += 1;
                continue;
            }
            Ok(folder::Removal::Failed(e)) => {
                skipped += 1;
                (ctx.emit)(Event::Warn {
                    key: "msg.emptyRemoveFailed".into(),
                    args: vec![
                        d.path.to_string_lossy().into_owned(),
                        tasks::short_reason(&e.to_string()),
                    ],
                });
                continue;
            }
            Ok(folder::Removal::Verified) => {
                // Handed on at once, with the fingerprint just verified, so the app moves it while
                // the check is fresh (and only if it still matches), not after the whole run.
                trash_one(ctx, &d.path, d.fingerprint().to_string());
            }
            Ok(folder::Removal::Removed) => {}
        }
        handed.push(d.path.clone());
        bytes += d.size;
        (ctx.emit)(Event::Step {
            done: i as u64 + 1,
            total: doomed.len() as u64,
        });
    }
    if handed.is_empty() {
        // Say when there was something to remove but every one of them was left alone.
        let (key, args) = if skipped > 0 {
            ("msg.duplicatesAllChanged", vec![skipped.to_string()])
        } else {
            ("msg.nothingToRemove", vec![])
        };
        return done(ctx, Outcome::Succeeded, Some(key), args);
    }
    let mut args = vec![handed.len().to_string(), bytes.to_string()];
    let key = match (permanent, skipped > 0) {
        (true, false) => "msg.deletedPermanently",
        (false, false) => "msg.recycled",
        (true, true) => "msg.deletedPermanentlyPartial",
        (false, true) => "msg.recycledPartial",
    };
    if skipped > 0 {
        args.push(skipped.to_string());
    }
    done(ctx, Outcome::Succeeded, Some(key), args);
}

/// Ask the app to move one checked item to the Trash (undoable; the engine never does it itself).
fn trash_one(ctx: &Ctx<'_>, path: &Path, fingerprint: String) {
    let path = path.to_string_lossy().into_owned();
    (ctx.emit)(Event::Needs {
        what: "trash".into(),
        paths: vec![path.clone()],
        items: vec![crate::protocol::NeedsItem { path, fingerprint }],
    });
}

/// Carry out a confirmed empty-folder plan. Every folder is re-checked right before it is handed on
/// (Trash) or removed (permanent): one that is not empty any more is left alone and reported with
/// `msg.emptySkipped` ([path]); one whose removal failed is reported with `msg.emptyRemoveFailed`
/// ([path, reason]). Nothing is ever deleted recursively.
fn remove_empty(ctx: &Ctx<'_>, planned: &[PathBuf], roots: &[PathBuf]) {
    let f = &ctx.settings.folder;
    let skipped_event = |p: &Path, why: Option<&str>| {
        let path = p.to_string_lossy().into_owned();
        let (key, args) = match why {
            None => ("msg.emptySkipped", vec![path]),
            Some(w) => ("msg.emptyRemoveFailed", vec![path, tasks::short_reason(w)]),
        };
        (ctx.emit)(Event::Warn {
            key: key.into(),
            args,
        });
    };
    if f.delete_method == DeleteMethod::Permanent {
        match ft::delete_empty_permanently(planned, roots, f) {
            Err(e) => fail(ctx, "msg.ioError", e.to_string()),
            Ok(out) => {
                for (p, why) in &out.skipped {
                    skipped_event(p, why.as_deref());
                }
                let skipped = out.skipped.len();
                match (out.removed, skipped) {
                    (0, 0) => done(ctx, Outcome::Succeeded, Some("msg.noEmpty"), vec![]),
                    (0, s) => done(
                        ctx,
                        Outcome::Succeeded,
                        Some("msg.emptyNoneRemoved"),
                        vec![s.to_string()],
                    ),
                    (n, 0) => done(
                        ctx,
                        Outcome::Succeeded,
                        Some("msg.emptyDeleted"),
                        vec![n.to_string()],
                    ),
                    (n, s) => done(
                        ctx,
                        Outcome::Succeeded,
                        Some("msg.emptyDeletedPartial"),
                        vec![n.to_string(), s.to_string()],
                    ),
                }
            }
        }
    } else {
        // One folder at a time: checked, then handed on with its (device, inode) right away.
        let (mut handed, mut total, mut skipped) = (0usize, 0usize, 0usize);
        for p in planned {
            let (keep, n, _) = ft::recheck_empty(std::slice::from_ref(p), f);
            match (keep.first(), folder::dir_id(p)) {
                (Some(_), Some(id)) => {
                    trash_one(ctx, p, crate::fsat::dir_fingerprint(id));
                    handed += 1;
                    total += n;
                }
                _ => {
                    skipped_event(p, None);
                    skipped += 1;
                }
            }
        }
        let (key, args) = match (handed, skipped) {
            (0, 0) => ("msg.noEmpty", vec![]),
            (0, s) => ("msg.emptyNoneRemoved", vec![s.to_string()]),
            (_, 0) => ("msg.emptyRecycled", vec![total.to_string()]),
            (_, s) => (
                "msg.emptyRecycledPartial",
                vec![total.to_string(), s.to_string()],
            ),
        };
        done(ctx, Outcome::Succeeded, Some(key), args);
    }
}

pub fn run_batch(task: &str, paths: &[String], ctx: &Ctx<'_>) {
    if tasks::check_task(task).is_err() {
        // Malformed or repeated option names: refuse the task rather than guess what was meant.
        return fail(ctx, "task.badOption", task.to_owned());
    }
    let (name, params) = parse_task(task);
    let roots = folders(paths);
    if roots.is_empty() {
        return fail(ctx, "msg.notAFolder", String::new());
    }
    let prog = Prog(ctx);
    let f = &ctx.settings.folder;
    match name.as_str() {
        "folder.tree" => match folder::render_tree(&roots, &f.tree, &prog) {
            Ok(text) => {
                (ctx.emit)(Event::Data {
                    name: "tree".into(),
                    data: json!({ "text": text }),
                });
                done(ctx, Outcome::Succeeded, None, vec![]);
            }
            Err(_) => cancelled(ctx),
        },
        "folder.analyze" => match ft::analyze(&roots, f, &prog) {
            Err(_) => cancelled(ctx),
            Ok(report) => {
                let summary: serde_json::Value = serde_json::from_slice(&ft::render_report(
                    &report,
                    crate::settings::ReportFormat::Json,
                ))
                .unwrap_or_default();
                (ctx.emit)(Event::Data {
                    name: "analyze".into(),
                    data: summary,
                });
                if params.get("save").is_some_and(|v| v == "1") {
                    let ext = ft::report_extension(f.analyze.save_format);
                    let bytes = ft::render_report(&report, f.analyze.save_format);
                    match tasks::write_result(
                        &roots[0],
                        &ctx.settings.output.report,
                        "_analysis",
                        Some(ext),
                        &bytes,
                    ) {
                        Ok(out) => {
                            (ctx.emit)(Event::Data {
                                name: "outputs".into(),
                                data: json!({ "paths": [out.to_string_lossy()] }),
                            });
                            (ctx.emit)(Event::Info {
                                key: "msg.reportSaved".into(),
                                args: vec![out.to_string_lossy().into_owned()],
                            });
                        }
                        Err(e) => return fail(ctx, e.key(), String::new()),
                    }
                }
                done(ctx, Outcome::Succeeded, None, vec![]);
            }
        },
        "folder.removeDuplicate" => {
            let mode = mode_from(&params, f.duplicates.keep);
            let started = Instant::now();
            let probe = mode.unwrap_or(DuplicateMode::KeepOldest);
            let (groups, plan) = match ft::plan_remove_duplicates(&roots, f, &prog, probe) {
                Ok(v) => v,
                Err(_) => return cancelled(ctx),
            };
            if groups.is_empty() {
                return done(ctx, Outcome::Succeeded, Some("msg.noDuplicates"), vec![]);
            }
            let Some(mode) = mode else {
                // Ask: report the groups; the app asks which copy to keep and runs the task again with `mode=`.
                let listing: Vec<_> = groups.iter().map(|g| g.iter().map(|x| json!({"path": x.path.to_string_lossy(), "size": x.size, "modified": x.modified.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)})).collect::<Vec<_>>()).collect();
                (ctx.emit)(Event::Data {
                    name: "duplicates".into(),
                    data: json!({"groups": listing, "scanSeconds": started.elapsed().as_secs_f64()}),
                });
                return done(
                    ctx,
                    Outcome::Succeeded,
                    Some("msg.duplicatesFound"),
                    vec![groups.len().to_string()],
                );
            };
            let _ = plan;
            // Groups in which anything changed since the scan are left alone (and reported).
            let (groups, changed) = folder::unchanged_groups(groups);
            for p in &changed {
                changed_event(ctx, p);
            }
            let mut doomed = folder::to_delete(&groups, mode);
            // A selected file is never removed (it was chosen, not found).
            let (selected, _) = selected_ids(paths);
            doomed.retain(|d| !selected.contains(&d.file_id));
            if !folder::plan_is_consistent(&groups, &doomed, mode) {
                // Never expected (the scan lists each file once); refuse rather than risk the only copy.
                return fail(ctx, "msg.ioError", "inconsistent duplicate plan".into());
            }
            if doomed.is_empty() {
                if !changed.is_empty() {
                    let n = changed.len().to_string();
                    return done(
                        ctx,
                        Outcome::Succeeded,
                        Some("msg.duplicatesAllChanged"),
                        vec![n],
                    );
                }
                return done(ctx, Outcome::Succeeded, Some("msg.nothingToRemove"), vec![]);
            }
            if !plan_holds_or_report(ctx, &groups, &doomed) {
                let n = doomed.len().to_string();
                return done(
                    ctx,
                    Outcome::Succeeded,
                    Some("msg.duplicatesAllChanged"),
                    vec![n],
                );
            }
            let bytes: u64 = doomed.iter().map(|d| d.size).sum();
            if params.get("dry").is_some_and(|v| v == "1") {
                // Preview only: say which copy stays and which go, delete nothing. The app asks the user
                // to confirm and then runs the task again without `dry`.
                let data = duplicate_preview(&groups, &doomed, mode, f.delete_method, bytes);
                (ctx.emit)(Event::Data {
                    name: "duplicates_preview".into(),
                    data,
                });
                return done(
                    ctx,
                    Outcome::Succeeded,
                    Some("msg.duplicatesPreview"),
                    vec![doomed.len().to_string(), bytes.to_string()],
                );
            }
            remove_duplicates(ctx, &groups, &doomed, mode, &roots);
        }
        "folder.removeEmpty" => {
            let (_, protected) = selected_ids(paths);
            let (plan, total) = match ft::plan_remove_empty_protecting(&roots, f, &prog, &protected)
            {
                Ok(v) => v,
                Err(_) => return cancelled(ctx),
            };
            if plan.paths.is_empty() {
                return done(ctx, Outcome::Succeeded, Some("msg.noEmpty"), vec![]);
            }
            if params.get("confirm").is_none_or(|v| v != "1") {
                (ctx.emit)(Event::Data {
                    name: "empty_folders".into(),
                    data: json!({"total": total, "targets": path_list(&plan.paths), "permanent": f.delete_method == DeleteMethod::Permanent}),
                });
                return done(
                    ctx,
                    Outcome::Succeeded,
                    Some("msg.emptyFound"),
                    vec![total.to_string()],
                );
            }
            remove_empty(ctx, &plan.paths, &roots);
        }
        "folder.rename" => {
            let plan = RenamePlan {
                prefix: params.get("prefix").cloned().unwrap_or_default(),
                name: params.get("name").cloned().unwrap_or_default(),
                suffix: params.get("suffix").cloned().unwrap_or_default(),
                numbered: params.get("numbered").is_none_or(|v| v != "0"),
            };
            if plan.validate().is_err() {
                return fail(ctx, "msg.invalidName", String::new());
            }
            let dry = match params.get("dry").map(String::as_str) {
                Some("1") => true,
                Some("0") => false,
                _ => f.rename.dry_run,
            };
            let excl = folder::ExclusionFilter::from_rules(&f.rename.exclusions);
            let (mut renamed, mut preview) = (0usize, Vec::new());
            for (i, root) in roots.iter().enumerate() {
                if (ctx.cancelled)() {
                    return cancelled(ctx);
                }
                (ctx.emit)(Event::Status {
                    key: "msg.processing".into(),
                    args: vec![root.to_string_lossy().into_owned()],
                });
                let steps = match ft::plan_rename(root, &plan, &f.rename, &excl) {
                    Ok(s) => s,
                    Err(e) => return fail(ctx, "msg.ioError", e.to_string()),
                };
                if dry {
                    preview.extend(ft::dry_run_lines(&steps));
                } else {
                    match ft::apply_rename(&steps, ctx.cancelled) {
                        Ok(out) => {
                            renamed += out.renamed;
                            for (n, why) in out.errors {
                                (ctx.emit)(Event::Error {
                                    key: "msg.renameFailed".into(),
                                    args: vec![n, why],
                                });
                            }
                        }
                        Err(_) => return cancelled(ctx),
                    }
                }
                (ctx.emit)(Event::Step {
                    done: i as u64 + 1,
                    total: roots.len() as u64,
                });
            }
            if dry {
                let n = preview.len().to_string();
                (ctx.emit)(Event::Data {
                    name: "rename_preview".into(),
                    data: json!({"lines": preview}),
                });
                done(ctx, Outcome::Succeeded, Some("msg.renameDryRun"), vec![n]);
            } else {
                done(
                    ctx,
                    Outcome::Succeeded,
                    Some("msg.renamed"),
                    vec![renamed.to_string()],
                );
            }
        }
        _ => fail(ctx, "task.unimplemented", task.to_owned()),
    }
}

/// Used by tests and the engine: where a result file would be written.
pub fn first_root(paths: &[String]) -> Option<&Path> {
    paths.first().map(Path::new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::sync::Mutex;

    fn run(task: &str, paths: &[&Path], settings: &Settings) -> Vec<serde_json::Value> {
        let events = Mutex::new(Vec::new());
        let emit = |e: Event| {
            events
                .lock()
                .unwrap()
                .push(serde_json::to_value(&e).unwrap())
        };
        let never = || false;
        let ctx = Ctx {
            settings,
            cancelled: &never,
            emit: &emit,
        };
        let p: Vec<String> = paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        run_batch(task, &p, &ctx);
        events.into_inner().unwrap()
    }

    /// Every path handed on for the Trash, over all `needs` events, with its fingerprint.
    fn trash_paths(ev: &[serde_json::Value]) -> Vec<String> {
        ev.iter()
            .filter(|e| e["event"] == "needs" && e["what"] == "trash")
            .flat_map(|e| {
                let items = e["items"].as_array().cloned().unwrap_or_default();
                let paths = e["paths"].as_array().cloned().unwrap_or_default();
                assert_eq!(items.len(), paths.len(), "{e}");
                for (i, p) in items.iter().zip(&paths) {
                    assert_eq!(&i["path"], p, "{e}");
                    let fp = i["fingerprint"].as_str().unwrap();
                    assert!(fp.starts_with("f:") || fp.starts_with("d:"), "{e}");
                }
                paths
            })
            .map(|p| p.as_str().unwrap().to_owned())
            .collect()
    }

    fn find<'a>(ev: &'a [serde_json::Value], kind: &str) -> Option<&'a serde_json::Value> {
        ev.iter().find(|e| e["event"] == kind)
    }

    fn setup() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        for (p, c) in [
            ("a/one.bin", "same"),
            ("b/two.bin", "same"),
            ("b/solo.txt", "solo"),
        ] {
            fs::create_dir_all(d.path().join(p).parent().unwrap()).unwrap();
            fs::write(d.path().join(p), c).unwrap();
        }
        fs::create_dir_all(d.path().join("hollow/inner")).unwrap();
        d
    }

    #[test]
    fn duplicates_ask_then_trash_request() {
        let d = setup();
        let s = Settings::default();
        let ev = run("folder.removeDuplicate", &[d.path()], &s);
        let data = find(&ev, "data").unwrap();
        assert_eq!(data["name"], "duplicates");
        assert_eq!(data["data"]["groups"].as_array().unwrap().len(), 1);
        assert_eq!(
            find(&ev, "done").unwrap()["summary_key"],
            "msg.duplicatesFound"
        );
        assert!(
            d.path().join("a/one.bin").exists() && d.path().join("b/two.bin").exists(),
            "asking deletes nothing"
        );

        let ev = run("folder.removeDuplicate?mode=newest", &[d.path()], &s);
        assert_eq!(trash_paths(&ev).len(), 1);
        let done = find(&ev, "done").unwrap();
        assert_eq!(
            (
                done["summary_key"].as_str(),
                done["summary_args"][0].as_str(),
                done["summary_args"][1].as_str()
            ),
            (Some("msg.recycled"), Some("1"), Some("4"))
        );
        assert!(
            d.path().join("a/one.bin").exists() && d.path().join("b/two.bin").exists(),
            "engine itself does not delete in Trash mode"
        );
    }

    #[test]
    fn duplicate_removal_previews_before_anything_is_moved() {
        let d = setup();
        // a second, unrelated duplicate pair, to see several groups
        fs::write(d.path().join("a/x.txt"), "pair").unwrap();
        fs::write(d.path().join("b/y.txt"), "pair").unwrap();
        for permanent in [false, true] {
            let mut s = Settings::default();
            if permanent {
                s.folder.delete_method = DeleteMethod::Permanent;
            }
            let ev = run("folder.removeDuplicate?mode=newest&dry=1", &[d.path()], &s);
            let data = find(&ev, "data").unwrap();
            assert_eq!(data["name"], "duplicates_preview");
            let p = &data["data"];
            assert_eq!(
                (
                    p["mode"].as_str(),
                    p["removeCount"].as_u64(),
                    p["reclaimBytes"].as_u64(),
                    p["permanent"].as_bool()
                ),
                (Some("newest"), Some(2), Some(8), Some(permanent))
            );
            let groups = p["groups"].as_array().unwrap();
            assert_eq!(groups.len(), 2);
            for g in groups {
                assert!(g["keep"]["path"].is_string());
                assert_eq!(g["remove"].as_array().unwrap().len(), 1);
                assert_ne!(g["keep"]["path"], g["remove"][0]["path"]);
            }
            let done = find(&ev, "done").unwrap();
            assert_eq!(done["summary_key"], "msg.duplicatesPreview");
            assert_eq!(done["summary_args"][0], "2");
            // nothing was asked of the app and nothing was deleted, even in permanent mode
            assert!(find(&ev, "needs").is_none());
            for f in ["a/one.bin", "b/two.bin", "a/x.txt", "b/y.txt"] {
                assert!(d.path().join(f).exists(), "{f}");
            }
        }
        // "delete all" keeps nothing
        let ev = run(
            "folder.removeDuplicate?mode=all&dry=1",
            &[d.path()],
            &Settings::default(),
        );
        let p = &find(&ev, "data").unwrap()["data"];
        assert_eq!(p["removeCount"], 4);
        assert!(p["groups"][0]["keep"].is_null());
        // the configured keep rule previews too; no duplicates / nothing to remove say so without data
        let mut s = Settings::default();
        s.folder.duplicates.keep = DuplicateKeep::Oldest;
        let ev = run("folder.removeDuplicate?dry=1", &[d.path()], &s);
        assert_eq!(find(&ev, "data").unwrap()["data"]["mode"], "oldest");
        // confirming = the same task without dry: only now does the app get the Trash request
        let ev = run(
            "folder.removeDuplicate?mode=newest",
            &[d.path()],
            &Settings::default(),
        );
        assert_eq!(trash_paths(&ev).len(), 2);
        let none = tempfile::tempdir().unwrap();
        fs::write(none.path().join("only.txt"), "u").unwrap();
        let ev = run(
            "folder.removeDuplicate?mode=newest&dry=1",
            &[none.path()],
            &Settings::default(),
        );
        assert!(find(&ev, "data").is_none());
        assert_eq!(
            find(&ev, "done").unwrap()["summary_key"],
            "msg.noDuplicates"
        );
    }

    #[test]
    fn duplicates_permanent_mode_deletes_inside_the_selection_only() {
        let d = setup();
        let mut s = Settings::default();
        s.folder.delete_method = DeleteMethod::Permanent;
        s.folder.duplicates.keep = DuplicateKeep::Oldest;
        let ev = run("folder.removeDuplicate", &[d.path()], &s);
        assert!(find(&ev, "needs").is_none());
        assert_eq!(
            find(&ev, "done").unwrap()["summary_key"],
            "msg.deletedPermanently"
        );
        let left = [d.path().join("a/one.bin"), d.path().join("b/two.bin")]
            .iter()
            .filter(|p| p.exists())
            .count();
        assert_eq!(left, 1);
        assert!(d.path().join("b/solo.txt").exists());
        assert_eq!(
            find(&run("folder.removeDuplicate", &[d.path()], &s), "done").unwrap()["summary_key"],
            "msg.noDuplicates"
        );
    }

    /// Selecting a folder together with a folder inside it (or the same folder twice, or the same
    /// folder under another spelling) once listed every file twice in one "duplicate" group, so a
    /// real run trashed / deleted the only copy. Hard links (two names, one inode) are one file too.
    #[test]
    fn overlapping_roots_and_hard_links_never_make_a_file_its_own_duplicate() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("A");
        let sub = a.join("sub");
        fs::create_dir_all(&sub).unwrap();
        let precious = sub.join("precious.txt");
        fs::write(&precious, "the only copy").unwrap();
        fs::hard_link(&precious, a.join("alias.txt")).unwrap();
        let canon_sub = fs::canonicalize(&sub).unwrap(); // /var/... -> /private/var/... on macOS
        let selections: Vec<Vec<&Path>> = vec![
            vec![&a, &sub],
            vec![&sub, &a],
            vec![&a, &a],
            vec![&a, &canon_sub],
            vec![&a],
        ];
        let mut permanent = Settings::default();
        permanent.folder.delete_method = DeleteMethod::Permanent;
        for roots in &selections {
            for mode in ["oldest", "newest", "all"] {
                let ev = run(
                    &format!("folder.removeDuplicate?mode={mode}&dry=1"),
                    roots,
                    &Settings::default(),
                );
                assert!(find(&ev, "data").is_none(), "{roots:?} {mode}: {ev:?}");
                assert_eq!(
                    find(&ev, "done").unwrap()["summary_key"],
                    "msg.noDuplicates",
                    "{roots:?} {mode}"
                );
                let ev = run(
                    &format!("folder.removeDuplicate?mode={mode}"),
                    roots,
                    &Settings::default(),
                );
                assert!(find(&ev, "needs").is_none(), "{roots:?} {mode}: {ev:?}");
                run(
                    &format!("folder.removeDuplicate?mode={mode}"),
                    roots,
                    &permanent,
                );
                assert!(precious.exists(), "{roots:?} {mode}: the only copy is gone");
                assert!(a.join("alias.txt").exists(), "{roots:?} {mode}");
            }
            // Ask mode lists no group either
            let ev = run("folder.removeDuplicate", roots, &Settings::default());
            assert!(find(&ev, "data").is_none(), "{roots:?}: {ev:?}");
        }
        // A real duplicate next to the overlap is still found exactly once.
        fs::write(a.join("copy.txt"), "the only copy").unwrap();
        let ev = run(
            "folder.removeDuplicate?mode=oldest&dry=1",
            &[&a, &sub],
            &Settings::default(),
        );
        let p = &find(&ev, "data").unwrap()["data"];
        assert_eq!(p["groups"].as_array().unwrap().len(), 1, "{p}");
        assert_eq!(p["removeCount"], 1, "{p}");
        let g = &p["groups"][0];
        assert_ne!(g["keep"]["path"], g["remove"][0]["path"]);
    }

    /// `/System/Volumes/Data<T>` and `<T>` are one folder (a firmlink; resolving the path does not
    /// undo it). A file touched between the two visits got two listing keys, both entries hashed
    /// equal, and "keep newest" permanently deleted the only copy.
    #[test]
    fn firmlink_spellings_never_make_a_file_its_own_duplicate() {
        let d = tempfile::tempdir().unwrap();
        let t = fs::canonicalize(d.path()).unwrap();
        let data = Path::new("/System/Volumes/Data").join(t.strip_prefix("/").unwrap());
        let same = |a: &Path, b: &Path| match (fs::metadata(a), fs::metadata(b)) {
            (Ok(x), Ok(y)) => (x.dev(), x.ino()) == (y.dev(), y.ino()),
            _ => false,
        };
        if !same(&data, &t) {
            eprintln!("no firmlink spelling of {t:?} here; skipped");
            return;
        }
        let precious = t.join("sub/only.txt");
        fs::create_dir_all(precious.parent().unwrap()).unwrap();
        fs::write(&precious, "the only copy").unwrap();
        let none = folder::NullProgress;

        // the roots collapse: one folder, two spellings (and one inside the other's spelling)
        assert_eq!(
            folder::distinct_roots(&[data.clone(), t.clone()], &none).unwrap(),
            std::slice::from_ref(&data)
        );
        assert_eq!(
            folder::distinct_roots(&[t.join("sub"), data.clone()], &none).unwrap(),
            std::slice::from_ref(&data)
        );

        // decision layer, with the double visit forced: each spelling scanned on its own and the
        // second visit seeing a later modification time (as after a background `touch`)
        let scan_one = |root: &Path| {
            folder::scan(
                &[root.to_path_buf()],
                &folder::ExclusionFilter::default(),
                &Default::default(),
                &none,
            )
            .unwrap()
            .files
        };
        let mut files = scan_one(&data);
        let mut second = scan_one(&t);
        assert_eq!((files.len(), second.len()), (1, 1));
        second[0].modified += std::time::Duration::from_secs(10);
        assert_ne!(files[0].same_file_key(), second[0].same_file_key());
        files.extend(second);
        let groups = folder::find_duplicates(&files, &Default::default(), &none).unwrap();
        assert!(groups.is_empty(), "{groups:?}");
        let forced = vec![files.clone()];
        for m in [
            DuplicateMode::KeepOldest,
            DuplicateMode::KeepNewest,
            DuplicateMode::DeleteAll,
        ] {
            assert!(folder::to_delete(&forced, m).is_empty(), "{m:?}");
            assert!(
                !folder::plan_is_consistent(&forced, &files[..1], m),
                "{m:?}"
            );
        }
        assert!(!folder::plan_still_holds(&forced, &files[..1]));

        // end to end, permanent deletion, every mode
        let mut perm = Settings::default();
        perm.folder.delete_method = DeleteMethod::Permanent;
        for mode in ["newest", "oldest", "all"] {
            for roots in [[&data, &t], [&t, &data]] {
                let roots: Vec<&Path> = roots.iter().map(|p| p.as_path()).collect();
                let ev = run(
                    &format!("folder.removeDuplicate?mode={mode}"),
                    &roots,
                    &perm,
                );
                assert_eq!(
                    find(&ev, "done").unwrap()["summary_key"],
                    "msg.noDuplicates",
                    "{mode} {roots:?}: {ev:?}"
                );
                assert!(precious.exists(), "{mode}: the only copy is gone");
            }
        }
    }

    /// Run the removal step of a duplicate plan made earlier: what happens when files change between
    /// the scan and the removal (the race the review hit with a background writer), made
    /// deterministic by changing them in between.
    fn remove_stale(
        dir: &Path,
        mode: DuplicateMode,
        settings: &Settings,
        between: impl FnOnce(&[folder::ScannedFile]),
    ) -> Vec<serde_json::Value> {
        let s = folder::scan(
            &[dir.to_path_buf()],
            &folder::ExclusionFilter::default(),
            &Default::default(),
            &folder::NullProgress,
        )
        .unwrap();
        let groups =
            folder::find_duplicates(&s.files, &Default::default(), &folder::NullProgress).unwrap();
        let doomed = folder::to_delete(&groups, mode);
        between(&doomed);
        let events = Mutex::new(Vec::new());
        let emit = |e: Event| {
            events
                .lock()
                .unwrap()
                .push(serde_json::to_value(&e).unwrap())
        };
        let never = || false;
        let ctx = Ctx {
            settings,
            cancelled: &never,
            emit: &emit,
        };
        remove_duplicates(&ctx, &groups, &doomed, mode, &[dir.to_path_buf()]);
        events.into_inner().unwrap()
    }

    fn changed_paths(ev: &[serde_json::Value]) -> Vec<String> {
        ev.iter()
            .filter(|e| e["event"] == "warn" && e["key"] == "msg.duplicateChanged")
            .map(|e| e["args"][0].as_str().unwrap().to_owned())
            .collect()
    }

    /// A plan that stops holding between the per-group check and the run (a file touched in that
    /// window, as legit.sh's background `touch` did) used to end the task with `msg.ioError`; it is
    /// reported per file and nothing is removed.
    #[test]
    fn a_plan_that_stops_holding_is_reported_not_an_error() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("a.txt"), "same").unwrap();
        fs::write(d.path().join("b.txt"), "same").unwrap();
        let s = folder::scan(
            &[d.path().to_path_buf()],
            &folder::ExclusionFilter::default(),
            &Default::default(),
            &folder::NullProgress,
        )
        .unwrap();
        let groups =
            folder::find_duplicates(&s.files, &Default::default(), &folder::NullProgress).unwrap();
        let doomed = folder::to_delete(&groups, DuplicateMode::KeepOldest);
        let events = Mutex::new(Vec::new());
        let emit = |e: Event| {
            events
                .lock()
                .unwrap()
                .push(serde_json::to_value(&e).unwrap())
        };
        let never = || false;
        let settings = Settings::default();
        let ctx = Ctx {
            settings: &settings,
            cancelled: &never,
            emit: &emit,
        };
        assert!(plan_holds_or_report(&ctx, &groups, &doomed));
        assert!(events.lock().unwrap().is_empty());
        let kept = groups[0].iter().find(|f| f.path != doomed[0].path).unwrap();
        fs::write(&kept.path, "same").unwrap(); // touched: new mtime / ctime, same content
        assert!(!plan_holds_or_report(&ctx, &groups, &doomed));
        let ev = events.into_inner().unwrap();
        assert_eq!(changed_paths(&ev), [kept.path.to_string_lossy()], "{ev:?}");
        assert!(ev.iter().all(|e| e["event"] != "error"), "{ev:?}");
    }

    /// rev021d swap.sh: after every check passed, a folder above the duplicate is swapped
    /// (`mv A A.old; mv B A`, or a link put in its place). Removing by path then deleted B/x.txt,
    /// the only copy of other content. Removal goes through the verified folder's handle, and the
    /// folder must still be where its path leads: nothing is removed, the file is reported.
    #[test]
    fn swapping_a_folder_after_the_checks_never_removes_another_file() {
        let mut perm = Settings::default();
        perm.folder.delete_method = DeleteMethod::Permanent;
        for variant in ["rename", "symlink", "before"] {
            let d = tempfile::tempdir().unwrap();
            let r = d.path().to_path_buf();
            for (p, c) in [
                ("keep/x.txt", "dup-content\n"),
                ("A/x.txt", "dup-content\n"),
                ("B/x.txt", "UNIQUE-ONLY-COPY\n"),
            ] {
                fs::create_dir_all(r.join(p).parent().unwrap()).unwrap();
                fs::write(r.join(p), c).unwrap();
            }
            let old =
                std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_577_836_800);
            fs::File::options()
                .write(true)
                .open(r.join("keep/x.txt"))
                .unwrap()
                .set_modified(old)
                .unwrap();
            let s = folder::scan(
                std::slice::from_ref(&r),
                &folder::ExclusionFilter::default(),
                &Default::default(),
                &folder::NullProgress,
            )
            .unwrap();
            let groups =
                folder::find_duplicates(&s.files, &Default::default(), &folder::NullProgress)
                    .unwrap();
            let doomed = folder::to_delete(&groups, DuplicateMode::KeepOldest);
            assert_eq!(doomed.len(), 1);
            assert_eq!(doomed[0].path, r.join("A/x.txt"));
            let swap = || {
                fs::rename(r.join("A"), r.join("A.old")).unwrap();
                if variant == "symlink" {
                    std::os::unix::fs::symlink("B", r.join("A")).unwrap();
                } else {
                    fs::rename(r.join("B"), r.join("A")).unwrap();
                }
            };
            if variant == "before" {
                swap();
            }
            let swapped = std::cell::Cell::new(false);
            let hook = |_: &folder::ScannedFile| {
                if variant != "before" && !swapped.replace(true) {
                    swap();
                }
            };
            let events = Mutex::new(Vec::new());
            let emit = |e: Event| {
                events
                    .lock()
                    .unwrap()
                    .push(serde_json::to_value(&e).unwrap())
            };
            let never = || false;
            let ctx = Ctx {
                settings: &perm,
                cancelled: &never,
                emit: &emit,
            };
            remove_duplicates_with(
                &ctx,
                &groups,
                &doomed,
                DuplicateMode::KeepOldest,
                std::slice::from_ref(&r),
                &hook,
            );
            let ev = events.into_inner().unwrap();
            assert!(
                variant == "before" || swapped.get(),
                "{variant}: the seam ran"
            );
            let unique = if variant == "symlink" {
                r.join("B/x.txt")
            } else {
                r.join("A/x.txt")
            };
            assert_eq!(
                fs::read_to_string(&unique).unwrap(),
                "UNIQUE-ONLY-COPY\n",
                "{variant}: {ev:?}"
            );
            assert_eq!(
                fs::read_to_string(r.join("A.old/x.txt")).unwrap(),
                "dup-content\n",
                "{variant}"
            );
            assert!(r.join("keep/x.txt").exists());
            assert_eq!(
                changed_paths(&ev),
                [r.join("A/x.txt").to_string_lossy()],
                "{variant}: {ev:?}"
            );
            assert_ne!(
                find(&ev, "done").unwrap()["summary_key"],
                "msg.deletedPermanently",
                "{variant}"
            );
        }
    }

    /// race.py "doomed" / "kept" / FAT: a copy that goes was rewritten in place, the copy that stays
    /// was replaced by an atomic save, or a file was recreated: the stale plan must not remove the
    /// only copy of anything. A legitimate duplicate is still removed.
    #[test]
    fn files_changed_after_the_scan_are_never_removed() {
        let mut perm = Settings::default();
        perm.folder.delete_method = DeleteMethod::Permanent;
        let make = || {
            let d = tempfile::tempdir().unwrap();
            fs::write(d.path().join("a.txt"), "original-0123456789\n").unwrap();
            fs::write(d.path().join("b.txt"), "original-0123456789\n").unwrap();
            // a.txt is older: KeepOldest keeps a.txt and removes b.txt
            let old =
                std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_577_836_800);
            fs::File::options()
                .write(true)
                .open(d.path().join("a.txt"))
                .unwrap()
                .set_modified(old)
                .unwrap();
            d
        };
        for settings in [&perm, &Settings::default()] {
            // 1. the copy that goes is rewritten in place (same size)
            let d = make();
            let ev = remove_stale(d.path(), DuplicateMode::KeepOldest, settings, |doomed| {
                assert_eq!(doomed.len(), 1);
                fs::write(&doomed[0].path, "NEW-WORK-ONLY-HERE!\n").unwrap();
            });
            assert_eq!(
                fs::read_to_string(d.path().join("b.txt")).unwrap(),
                "NEW-WORK-ONLY-HERE!\n"
            );
            assert_eq!(
                changed_paths(&ev),
                [d.path().join("b.txt").to_string_lossy()],
                "{ev:?}"
            );
            assert!(find(&ev, "needs").is_none(), "{ev:?}");
            let done = find(&ev, "done").unwrap();
            assert_eq!(
                done["summary_key"], "msg.duplicatesAllChanged",
                "every candidate skipped"
            );
            assert_eq!(done["summary_args"], json!(["1"]));

            // 2. the copy that stays is replaced by an atomic save with other content
            let d = make();
            let ev = remove_stale(d.path(), DuplicateMode::KeepOldest, settings, |_| {
                fs::write(d.path().join(".tmp"), "version-2-different\n").unwrap();
                fs::rename(d.path().join(".tmp"), d.path().join("a.txt")).unwrap();
            });
            assert_eq!(
                fs::read_to_string(d.path().join("b.txt")).unwrap(),
                "original-0123456789\n",
                "the last copy of the original"
            );
            assert_eq!(changed_paths(&ev).len(), 1, "{ev:?}");
            assert!(find(&ev, "needs").is_none());

            // 3. the copy that stays changes content but keeps its inode and size (in-place write)
            let d = make();
            let ev = remove_stale(d.path(), DuplicateMode::KeepOldest, settings, |_| {
                fs::write(d.path().join("a.txt"), "ORIGINAL-0123456789\n").unwrap();
            });
            assert!(d.path().join("b.txt").exists());
            assert_eq!(changed_paths(&ev).len(), 1, "{ev:?}");

            // 4. a removed copy recreated under the same name (inode reused on FAT): ctime differs
            let d = make();
            let ev = remove_stale(d.path(), DuplicateMode::DeleteAll, settings, |doomed| {
                fs::remove_file(&doomed[1].path).unwrap();
                fs::write(&doomed[1].path, "original-0123456789\n").unwrap();
            });
            assert_eq!(changed_paths(&ev).len(), 1, "{ev:?}");

            // 5. nothing changed: the duplicate goes (or is handed to the Trash)
            let d = make();
            let ev = remove_stale(d.path(), DuplicateMode::KeepOldest, settings, |_| {});
            assert!(changed_paths(&ev).is_empty(), "{ev:?}");
            if std::ptr::eq(settings, &perm) {
                assert!(!d.path().join("b.txt").exists());
                assert_eq!(
                    find(&ev, "done").unwrap()["summary_key"],
                    "msg.deletedPermanently"
                );
            } else {
                assert_eq!(trash_paths(&ev), [d.path().join("b.txt").to_string_lossy()]);
            }
            assert!(d.path().join("a.txt").exists());
        }
    }

    /// A selected item is never planned for removal: `--apply D D/chosen.txt` handed the selected
    /// file to the Trash, and permanent mode removed a selected empty folder that Trash mode refused.
    #[test]
    fn selected_items_are_never_removed_in_either_mode() {
        let mut perm = Settings::default();
        perm.folder.delete_method = DeleteMethod::Permanent;
        for settings in [&Settings::default(), &perm] {
            let d = tempfile::tempdir().unwrap();
            let r = d.path();
            fs::write(r.join("copy.txt"), "same").unwrap();
            fs::write(r.join("chosen.txt"), "same").unwrap();
            let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
            fs::File::options()
                .write(true)
                .open(r.join("copy.txt"))
                .unwrap()
                .set_modified(old)
                .unwrap();
            let chosen = r.join("chosen.txt");
            for mode in ["oldest", "all"] {
                let ev = run(
                    &format!("folder.removeDuplicate?mode={mode}&dry=1"),
                    &[r, &chosen],
                    settings,
                );
                if let Some(data) = find(&ev, "data") {
                    for g in data["data"]["groups"].as_array().unwrap() {
                        for x in g["remove"].as_array().unwrap() {
                            let p = x["path"].as_str().unwrap();
                            assert!(!p.ends_with("chosen.txt"), "{mode}: {ev:?}");
                        }
                    }
                }
                let ev = run(
                    &format!("folder.removeDuplicate?mode={mode}"),
                    &[r, &chosen],
                    settings,
                );
                assert!(
                    !trash_paths(&ev).iter().any(|p| p.ends_with("chosen.txt")),
                    "{mode}: {ev:?}"
                );
                assert!(chosen.exists(), "{mode}: {ev:?}");
            }

            // empty folders: a selected one, and a folder holding only a selected one
            let e = r.join("E");
            let inner = r.join("X/inner");
            fs::create_dir_all(&e).unwrap();
            fs::create_dir_all(&inner).unwrap();
            fs::create_dir_all(r.join("free")).unwrap();
            let listed = run("folder.removeEmpty", &[r, &e, &inner], settings);
            let targets = &find(&listed, "data").unwrap()["data"]["targets"];
            assert_eq!(
                targets,
                &json!([r.join("free").to_string_lossy()]),
                "{listed:?}"
            );
            let ev = run("folder.removeEmpty?confirm=1", &[r, &e, &inner], settings);
            assert!(e.is_dir() && inner.is_dir(), "{ev:?}");
            let t = trash_paths(&ev);
            if !t.is_empty() {
                assert_eq!(t, [r.join("free").to_string_lossy()]);
            } else {
                assert!(!r.join("free").exists(), "{ev:?}");
            }
        }
    }

    /// Runs that leave something alone say so: a partial summary with the count, and a cancelled
    /// permanent run says how many were already deleted.
    #[test]
    fn partial_and_cancelled_runs_report_their_counts() {
        let mut perm = Settings::default();
        perm.folder.delete_method = DeleteMethod::Permanent;
        let make = || {
            let d = tempfile::tempdir().unwrap();
            for n in ["a", "b", "c"] {
                fs::write(d.path().join(format!("{n}1.txt")), n).unwrap();
                fs::write(d.path().join(format!("{n}2.txt")), n).unwrap();
            }
            d
        };
        // one of three duplicates changes after the checks of the others: partial
        for (settings, key) in [
            (&perm, "msg.deletedPermanentlyPartial"),
            (&Settings::default(), "msg.recycledPartial"),
        ] {
            let d = make();
            let ev = remove_stale(d.path(), DuplicateMode::KeepOldest, settings, |doomed| {
                assert_eq!(doomed.len(), 3);
                fs::write(&doomed[1].path, "x").unwrap();
            });
            let done = find(&ev, "done").unwrap();
            assert_eq!(done["summary_key"], key, "{ev:?}");
            assert_eq!(done["summary_args"][0], "2");
            assert_eq!(done["summary_args"][2], "1");
        }
        // cancelled after the first removal
        let d = make();
        let s = folder::scan(
            &[d.path().to_path_buf()],
            &folder::ExclusionFilter::default(),
            &Default::default(),
            &folder::NullProgress,
        )
        .unwrap();
        let groups =
            folder::find_duplicates(&s.files, &Default::default(), &folder::NullProgress).unwrap();
        let doomed = folder::to_delete(&groups, DuplicateMode::KeepOldest);
        let events = Mutex::new(Vec::new());
        let emit = |e: Event| {
            events
                .lock()
                .unwrap()
                .push(serde_json::to_value(&e).unwrap())
        };
        let stop = std::sync::atomic::AtomicBool::new(false);
        let cancel = || stop.load(std::sync::atomic::Ordering::SeqCst);
        let ctx = Ctx {
            settings: &perm,
            cancelled: &cancel,
            emit: &emit,
        };
        let hook = |_: &folder::ScannedFile| stop.store(true, std::sync::atomic::Ordering::SeqCst);
        remove_duplicates_with(
            &ctx,
            &groups,
            &doomed,
            DuplicateMode::KeepOldest,
            &[d.path().to_path_buf()],
            &hook,
        );
        let ev = events.into_inner().unwrap();
        let done = find(&ev, "done").unwrap();
        assert_eq!(done["outcome"], "cancelled", "{ev:?}");
        assert_eq!(done["summary_key"], "msg.cancelledAfterDeleting");
        assert_eq!(done["summary_args"][0], "1");
        assert_eq!(
            fs::read_dir(d.path()).unwrap().count(),
            5,
            "exactly one removed"
        );
    }

    /// `kept_of` searched every group for every doomed file (16,000 pairs: ~34 s in debug).
    #[test]
    fn kept_copies_are_found_in_one_pass() {
        let t = |s| std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(s);
        let f = |i: u64, s| folder::ScannedFile {
            path: PathBuf::from(format!("/x/{i}")),
            size: 1,
            modified: t(s),
            extension: String::new(),
            file_id: (1, i),
            changed: (0, 0),
            mtime: (0, 0),
            parent_id: (0, 0),
            root: Default::default(),
        };
        let groups: Vec<Vec<folder::ScannedFile>> = (0..16_000u64)
            .map(|i| vec![f(2 * i, 1), f(2 * i + 1, 2)])
            .collect();
        let doomed = folder::to_delete(&groups, DuplicateMode::KeepOldest);
        let start = Instant::now();
        let kept = kept_copies(&groups, &doomed, DuplicateMode::KeepOldest);
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "{:?}",
            start.elapsed()
        );
        assert_eq!(kept.len(), 16_000);
        for (i, d) in doomed.iter().enumerate() {
            assert_eq!(kept[d.path.as_path()].path, groups[i][0].path);
        }
        assert!(kept_copies(&groups, &doomed, DuplicateMode::DeleteAll).is_empty());
    }

    /// A selected link to a file or folder inside a selected folder protects its target too: with
    /// D plus `Lnk -> D/chosen.txt` selected, chosen.txt was removed.
    #[test]
    fn a_selected_link_protects_its_target() {
        let mut perm = Settings::default();
        perm.folder.delete_method = DeleteMethod::Permanent;
        for settings in [&Settings::default(), &perm] {
            let top = tempfile::tempdir().unwrap();
            let d = top.path().join("D");
            fs::create_dir_all(d.join("E")).unwrap();
            fs::write(d.join("copy.txt"), "same").unwrap();
            fs::write(d.join("chosen.txt"), "same").unwrap();
            let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
            fs::File::options()
                .write(true)
                .open(d.join("copy.txt"))
                .unwrap()
                .set_modified(old)
                .unwrap();
            let (lf, ld) = (top.path().join("Lnk"), top.path().join("LnkE"));
            std::os::unix::fs::symlink(d.join("chosen.txt"), &lf).unwrap();
            std::os::unix::fs::symlink(d.join("E"), &ld).unwrap();
            for mode in ["oldest", "all"] {
                let ev = run(
                    &format!("folder.removeDuplicate?mode={mode}"),
                    &[&d, &lf],
                    settings,
                );
                assert!(d.join("chosen.txt").exists(), "{mode}: {ev:?}");
                assert!(
                    !trash_paths(&ev).iter().any(|p| p.contains("chosen.txt")),
                    "{mode}: {ev:?}"
                );
            }
            let listed = run("folder.removeEmpty", &[&d, &ld], settings);
            assert!(find(&listed, "data").is_none(), "{listed:?}");
            run("folder.removeEmpty?confirm=1", &[&d, &ld], settings);
            assert!(d.join("E").is_dir());
        }
    }

    /// The plan is looked at again right before it is carried out.
    #[test]
    fn a_plan_that_no_longer_holds_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let (a, b) = (d.path().join("a.txt"), d.path().join("b.txt"));
        fs::write(&a, "same").unwrap();
        fs::write(&b, "same").unwrap();
        let s = folder::scan(
            &[d.path().to_path_buf()],
            &folder::ExclusionFilter::default(),
            &Default::default(),
            &folder::NullProgress,
        )
        .unwrap();
        let groups =
            folder::find_duplicates(&s.files, &Default::default(), &folder::NullProgress).unwrap();
        let doomed = folder::to_delete(&groups, DuplicateMode::KeepOldest);
        assert_eq!(doomed.len(), 1);
        assert!(folder::plan_still_holds(&groups, &doomed));
        let kept = if doomed[0].path == a { &b } else { &a };
        // the copy that goes is now a hard link to the copy that stays
        fs::remove_file(&doomed[0].path).unwrap();
        fs::hard_link(kept, &doomed[0].path).unwrap();
        assert!(!folder::plan_still_holds(&groups, &doomed));
        // the copy that stays is gone
        fs::remove_file(&doomed[0].path).unwrap();
        fs::write(&doomed[0].path, "same").unwrap();
        fs::remove_file(kept).unwrap();
        assert!(!folder::plan_still_holds(&groups, &doomed));
    }

    /// An excluded subfolder was skipped by the scan, so its parent looked empty; permanent mode then
    /// fell back to `remove_dir_all` and deleted `node_modules/lib.js`, and Trash mode trashed the tree.
    #[test]
    fn excluded_or_unreadable_folders_are_content_and_never_deleted() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        let lib = r.join("B/keep/node_modules/lib.js");
        fs::create_dir_all(lib.parent().unwrap()).unwrap();
        fs::write(&lib, "module.exports = 1").unwrap();
        fs::create_dir_all(r.join("B/hollow")).unwrap();
        fs::create_dir_all(r.join("litter")).unwrap();
        fs::write(r.join("litter/.DS_Store"), "x").unwrap();
        fs::write(r.join("litter/._x"), "x").unwrap();
        // a zero-byte file next to real content must not make it "empty", even with the toggle on
        fs::create_dir_all(r.join("zero/node_modules")).unwrap();
        fs::write(r.join("zero/marker"), "").unwrap();
        // an unreadable folder is content too
        let locked = r.join("C/locked");
        fs::create_dir_all(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let mut s = Settings::default();
        s.folder.scan_exclusions.folders = "node_modules".into();
        s.folder.empty.ignore_zero_byte_files = true;
        let mut perm = s.clone();
        perm.folder.delete_method = DeleteMethod::Permanent;

        let listed = run("folder.removeEmpty", &[r], &s);
        let targets: Vec<String> = find(&listed, "data").unwrap()["data"]["targets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.as_str().unwrap().to_owned())
            .collect();
        let want: Vec<String> = ["B/hollow", "litter"]
            .iter()
            .map(|p| r.join(p).to_string_lossy().into_owned())
            .collect();
        assert_eq!(targets, want, "{listed:?}");

        let ev = run("folder.removeEmpty?confirm=1", &[r], &s);
        assert_eq!(trash_paths(&ev), want);

        let ev = run("folder.removeEmpty?confirm=1", &[r], &perm);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            find(&ev, "done").unwrap()["summary_key"],
            "msg.emptyDeleted",
            "{ev:?}"
        );
        assert!(lib.exists(), "excluded content was deleted: {ev:?}");
        assert!(r.join("zero/marker").exists() && locked.exists());
        assert!(!r.join("B/hollow").exists() && !r.join("litter").exists());
    }

    /// A permanent removal that fails is reported with its reason ([path, reason]); the reason used
    /// to be sent as a second argument of `msg.emptySkipped`, whose text never showed it.
    #[test]
    fn failed_permanent_removal_reports_the_reason() {
        let d = tempfile::tempdir().unwrap();
        let parent = d.path().join("P");
        fs::create_dir_all(parent.join("E")).unwrap();
        // P holds only an empty folder, so P is planned; E cannot be removed from a read-only P.
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o555)).unwrap();
        let mut perm = Settings::default();
        perm.folder.delete_method = DeleteMethod::Permanent;
        let ev = run("folder.removeEmpty?confirm=1", &[d.path()], &perm);
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
        let warn = ev
            .iter()
            .find(|e| e["event"] == "warn")
            .unwrap_or_else(|| panic!("{ev:?}"));
        assert_eq!(warn["key"], "msg.emptyRemoveFailed", "{ev:?}");
        let args = warn["args"].as_array().unwrap();
        assert_eq!(args.len(), 2, "{warn}");
        assert_eq!(args[0], parent.to_string_lossy().as_ref());
        let why = args[1].as_str().unwrap();
        assert!(
            !why.is_empty() && !why.contains(char::is_control),
            "{why:?}"
        );
        assert!(parent.join("E").exists());
        let done = find(&ev, "done").unwrap();
        assert_eq!(done["summary_key"], "msg.emptyNoneRemoved", "{ev:?}");
        assert_eq!(done["summary_args"], json!(["1"]));
    }

    /// Option names are never percent-decoded: `%63onfirm` is not `confirm` (the CLI guard matched
    /// the raw name, the engine the decoded one). A malformed name rejects the whole task.
    #[test]
    fn encoded_option_names_are_rejected_not_decoded() {
        let d = setup();
        let mut s = Settings::default();
        s.folder.delete_method = DeleteMethod::Permanent;
        for task in [
            "folder.removeEmpty?%63onfirm=1",
            "folder.removeEmpty?confirm%3D1",
            "folder.removeEmpty?+confirm=1",
            "folder.removeEmpty?1confirm=1",
            "folder.removeEmpty?confirm=1&confirm=1",
            "folder.removeDuplicate?mode=all&%64ry=0",
        ] {
            let ev = run(task, &[d.path()], &s);
            assert!(d.path().join("hollow").is_dir(), "{task}: {ev:?}");
            assert!(d.path().join("a/one.bin").exists() && d.path().join("b/two.bin").exists());
            assert_eq!(
                find(&ev, "error").unwrap()["key"],
                "task.badOption",
                "{task}"
            );
            assert_eq!(find(&ev, "done").unwrap()["outcome"], "failed", "{task}");
        }
        // plain names and encoded values still work
        let ev = run("folder.removeEmpty?confirm=%31", &[d.path()], &s);
        assert_eq!(
            find(&ev, "done").unwrap()["summary_key"],
            "msg.emptyDeleted"
        );
    }

    #[test]
    fn empty_folders_need_confirmation() {
        let d = setup();
        let s = Settings::default();
        let ev = run("folder.removeEmpty", &[d.path()], &s);
        assert_eq!(find(&ev, "data").unwrap()["data"]["total"], 2);
        assert_eq!(find(&ev, "done").unwrap()["summary_key"], "msg.emptyFound");
        assert!(d.path().join("hollow").exists());
        let ev = run("folder.removeEmpty?confirm=1", &[d.path()], &s);
        let t = trash_paths(&ev);
        assert_eq!(t.len(), 1, "only the outermost empty folder is trashed");
        assert!(t[0].ends_with("/hollow"));
    }

    #[test]
    fn rename_flow_with_encoded_parameters_and_dry_run() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("b.png"), "").unwrap();
        fs::write(d.path().join("a.png"), "").unwrap();
        let s = Settings::default();
        let ev = run(
            "folder.rename?prefix=%E6%97%85%E8%A1%8C_&numbered=1&dry=1",
            &[d.path()],
            &s,
        );
        let lines = find(&ev, "data").unwrap()["data"]["lines"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(lines[0], "a.png  ->  旅行_1.png");
        assert!(d.path().join("a.png").exists(), "dry run renames nothing");
        let ev = run(
            "folder.rename?prefix=%E6%97%85%E8%A1%8C_&numbered=1&dry=0",
            &[d.path()],
            &s,
        );
        assert_eq!(find(&ev, "done").unwrap()["summary_args"][0], "2");
        assert!(d.path().join("旅行_1.png").exists() && d.path().join("旅行_2.png").exists());
        let bad = run("folder.rename?prefix=a%2Fb&dry=0", &[d.path()], &s);
        assert_eq!(find(&bad, "error").unwrap()["key"], "msg.invalidName");
    }

    #[test]
    fn tree_analyze_and_bad_selection() {
        let d = setup();
        let s = Settings::default();
        let ev = run("folder.tree", &[d.path()], &s);
        assert!(find(&ev, "data").unwrap()["data"]["text"]
            .as_str()
            .unwrap()
            .contains("├───a"));
        let ev = run("folder.analyze?save=1", &[d.path()], &s);
        assert_eq!(find(&ev, "data").unwrap()["data"]["files"], 3);
        assert!(find(&ev, "info").unwrap()["args"][0]
            .as_str()
            .unwrap()
            .ends_with("_analysis.csv"));
        assert!(d
            .path()
            .with_file_name(format!(
                "{}_analysis.csv",
                d.path().file_name().unwrap().to_string_lossy()
            ))
            .exists());
        let _ = fs::remove_file(d.path().with_file_name(format!(
            "{}_analysis.csv",
            d.path().file_name().unwrap().to_string_lossy()
        )));
        let file = d.path().join("b/solo.txt");
        assert_eq!(
            find(&run("folder.tree", &[&file], &s), "error").unwrap()["key"],
            "msg.notAFolder"
        );
    }

    #[test]
    fn cancelled_batch_reports_cancelled() {
        let d = setup();
        let s = Settings::default();
        let events = Mutex::new(Vec::new());
        let emit = |e: Event| {
            events
                .lock()
                .unwrap()
                .push(serde_json::to_value(&e).unwrap())
        };
        let yes = || true;
        run_batch(
            "folder.analyze",
            &[d.path().to_string_lossy().into_owned()],
            &Ctx {
                settings: &s,
                cancelled: &yes,
                emit: &emit,
            },
        );
        let ev = events.into_inner().unwrap();
        assert_eq!(find(&ev, "done").unwrap()["outcome"], "cancelled");
    }
}
