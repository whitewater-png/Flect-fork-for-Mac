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

/// Hand a delete plan to wherever it is carried out, and say what happened.
fn carry_out(
    ctx: &Ctx<'_>,
    paths: Vec<PathBuf>,
    roots: &[PathBuf],
    summary_trash: &str,
    summary_perm: &str,
    args: Vec<String>,
) {
    if ctx.settings.folder.delete_method == DeleteMethod::Permanent {
        match ft::delete_permanently(&paths, roots) {
            Ok(_) => done(ctx, Outcome::Succeeded, Some(summary_perm), args),
            Err(e) => fail(ctx, "msg.ioError", e.to_string()),
        }
    } else {
        // The app moves these to the Trash (undoable); the engine never does it itself.
        (ctx.emit)(Event::Needs {
            what: "trash".into(),
            paths: path_list(&paths),
        });
        done(ctx, Outcome::Succeeded, Some(summary_trash), args);
    }
}

/// Carry out a confirmed empty-folder plan. Every folder is re-checked right before it is handed on
/// (Trash) or removed (permanent): one that is not empty any more is left alone and reported with
/// `msg.emptySkipped`; nothing is ever deleted recursively.
fn remove_empty(ctx: &Ctx<'_>, planned: &[PathBuf], roots: &[PathBuf]) {
    let f = &ctx.settings.folder;
    let skipped_event = |p: &Path, why: Option<String>| {
        let mut args = vec![p.to_string_lossy().into_owned()];
        args.extend(why);
        (ctx.emit)(Event::Warn {
            key: "msg.emptySkipped".into(),
            args,
        });
    };
    if f.delete_method == DeleteMethod::Permanent {
        match ft::delete_empty_permanently(planned, roots, f) {
            Err(e) => fail(ctx, "msg.ioError", e.to_string()),
            Ok(out) => {
                for (p, why) in &out.skipped {
                    skipped_event(p, Some(why.clone()));
                }
                if out.removed == 0 {
                    return done(ctx, Outcome::Succeeded, Some("msg.noEmpty"), vec![]);
                }
                done(
                    ctx,
                    Outcome::Succeeded,
                    Some("msg.emptyDeleted"),
                    vec![out.removed.to_string()],
                );
            }
        }
    } else {
        let (keep, total, skipped) = ft::recheck_empty(planned, f);
        for p in &skipped {
            skipped_event(p, None);
        }
        if keep.is_empty() {
            return done(ctx, Outcome::Succeeded, Some("msg.noEmpty"), vec![]);
        }
        (ctx.emit)(Event::Needs {
            what: "trash".into(),
            paths: path_list(&keep),
        });
        done(
            ctx,
            Outcome::Succeeded,
            Some("msg.emptyRecycled"),
            vec![total.to_string()],
        );
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
            let doomed = folder::to_delete(&groups, mode);
            if !folder::plan_is_consistent(&groups, &doomed, mode) {
                // Never expected (the scan lists each file once); refuse rather than risk the only copy.
                return fail(ctx, "msg.ioError", "inconsistent duplicate plan".into());
            }
            if doomed.is_empty() {
                return done(ctx, Outcome::Succeeded, Some("msg.nothingToRemove"), vec![]);
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
            let count = doomed.len().to_string();
            carry_out(
                ctx,
                doomed.into_iter().map(|d| d.path).collect(),
                &roots,
                "msg.recycled",
                "msg.deletedPermanently",
                vec![count, bytes.to_string()],
            );
        }
        "folder.removeEmpty" => {
            let (plan, total) = match ft::plan_remove_empty(&roots, f, &prog) {
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
    use std::os::unix::fs::PermissionsExt;
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
        let needs = find(&ev, "needs").unwrap();
        assert_eq!(needs["what"], "trash");
        assert_eq!(needs["paths"].as_array().unwrap().len(), 1);
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
        assert_eq!(
            find(&ev, "needs").unwrap()["paths"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
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
        let trash: Vec<&str> = find(&ev, "needs").unwrap()["paths"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p.as_str().unwrap())
            .collect();
        assert_eq!(trash, want.iter().map(String::as_str).collect::<Vec<_>>());

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
        let t = find(&ev, "needs").unwrap();
        assert_eq!(
            t["paths"].as_array().unwrap().len(),
            1,
            "only the outermost empty folder is trashed"
        );
        assert!(t["paths"][0].as_str().unwrap().ends_with("/hollow"));
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
