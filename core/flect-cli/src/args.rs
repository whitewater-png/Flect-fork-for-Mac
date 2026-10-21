//! Hand-rolled argument parsing (no dependency needed for this small grammar).
use std::ffi::OsString;
use std::path::PathBuf;

use serde_json::Value;

pub const USAGE: &str = "\
Usage:
  flect list [--json]
  flect run <task> [--option key=value ...] [--json-options '{...}'] [--json] <paths...>
  flect run --preset <name> [--json] <paths...>
  flect run folder.removeDuplicate|folder.removeEmpty ... [--apply [--permanent]] <paths...>
  flect settings path
  flect --help | --version

Tasks come from `flect list`. Options are passed to the task like the app does,
e.g. `flect run image.resize -o scale=50 photo.png`. `--preset` runs a preset saved in
the app (its name, or its id), e.g. `flect run --preset \"SNS 1080px / 2MB\" photo.png`.
Deleting tasks (folder.removeDuplicate, folder.removeEmpty) only preview by default; --apply
really deletes, and with the \"delete permanently\" setting --permanent is required as well.
Exit status: 0 success, 1 failed, 2 usage error, 130 cancelled (Ctrl-C).";

#[derive(Debug, PartialEq)]
pub enum Command {
    Help,
    Version,
    List { json: bool },
    SettingsPath,
    Run(RunArgs),
}

#[derive(Debug, PartialEq)]
pub struct RunArgs {
    /// Task id plus its encoded query, as the engine's `run.task` expects (`ID?k=v&k=v`).
    /// Empty when `preset` is set.
    pub task: String,
    /// `--preset NAME`: a preset from `presets.json` instead of a task.
    pub preset: Option<String>,
    /// Absolute paths, in the order given (order matters for `pdf.merge`).
    pub paths: Vec<String>,
    pub json: bool,
    /// `--apply`: let the deleting tasks really delete (default: preview only).
    pub apply: bool,
    /// `--permanent`: confirm deletion without the Trash when the setting is "delete permanently".
    pub permanent: bool,
}

/// Parse `argv[1..]`. `cwd` makes relative paths absolute (the engine accepts absolute paths only).
pub fn parse(args: Vec<OsString>, cwd: &std::path::Path) -> Result<Command, String> {
    let args: Vec<String> = args
        .into_iter()
        .map(|a| {
            a.into_string()
                .map_err(|_| "arguments must be valid UTF-8".to_owned())
        })
        .collect::<Result<_, _>>()?;
    let mut it = args.iter().map(String::as_str);
    match it.next() {
        None => Err("missing command".into()),
        Some("-h" | "--help" | "help") => Ok(Command::Help),
        Some("-V" | "--version") => Ok(Command::Version),
        Some("list") => {
            let mut json = false;
            for a in it {
                match a {
                    "--json" => json = true,
                    other => return Err(format!("unexpected argument for list: {other}")),
                }
            }
            Ok(Command::List { json })
        }
        Some("settings") => match (it.next(), it.next()) {
            (Some("path"), None) => Ok(Command::SettingsPath),
            _ => Err("usage: flect settings path".into()),
        },
        Some("run") => parse_run(it, cwd),
        Some(other) => Err(format!("unknown command: {other}")),
    }
}

fn parse_run<'a>(
    mut it: impl Iterator<Item = &'a str>,
    cwd: &std::path::Path,
) -> Result<Command, String> {
    let (mut json, mut apply, mut permanent) = (false, false, false);
    let mut options: Vec<(String, String)> = vec![];
    let mut positional: Vec<&str> = vec![];
    let mut preset: Option<String> = None;
    while let Some(a) = it.next() {
        match a {
            "--" => {
                positional.extend(it.by_ref());
                break;
            }
            "--json" => json = true,
            "--apply" => apply = true,
            "--permanent" => permanent = true,
            "--preset" => preset = Some(it.next().ok_or("--preset needs a name")?.to_owned()),
            _ if a.starts_with("--preset=") => preset = Some(a["--preset=".len()..].to_owned()),
            "-o" | "--option" => {
                let v = it.next().ok_or("--option needs key=value")?;
                options.push(parse_option(v)?);
            }
            "--json-options" => {
                let v = it.next().ok_or("--json-options needs a JSON object")?;
                options.extend(parse_json_options(v)?);
            }
            _ if a.starts_with("--option=") => options.push(parse_option(&a["--option=".len()..])?),
            _ if a.starts_with("--json-options=") => {
                options.extend(parse_json_options(&a["--json-options=".len()..])?)
            }
            _ if a.starts_with('-') && a.len() > 1 => return Err(format!("unknown option: {a}")),
            _ => positional.push(a),
        }
    }
    if let Some(name) = preset {
        if name.is_empty() {
            return Err("--preset needs a name".into());
        }
        if !options.is_empty() {
            return Err("--preset cannot be combined with --option / --json-options".into());
        }
        let paths: Vec<String> = positional
            .into_iter()
            .map(|p| absolute(p, cwd))
            .collect::<Result<_, _>>()?;
        if paths.is_empty() {
            return Err("missing path(s)".into());
        }
        return Ok(Command::Run(RunArgs {
            task: String::new(),
            preset: Some(name),
            paths,
            json,
            apply,
            permanent,
        }));
    }
    let mut positional = positional.into_iter();
    let task = positional.next().ok_or("missing task (see `flect list`)")?;
    let paths: Vec<String> = positional
        .map(|p| absolute(p, cwd))
        .collect::<Result<_, _>>()?;
    if paths.is_empty() {
        return Err("missing path(s)".into());
    }
    Ok(Command::Run(RunArgs {
        task: build_task(task, &options)?,
        preset: None,
        paths,
        json,
        apply,
        permanent,
    }))
}

fn parse_option(s: &str) -> Result<(String, String), String> {
    let (k, v) = s
        .split_once('=')
        .ok_or_else(|| format!("option must be key=value: {s}"))?;
    check_key(k)?;
    Ok((k.to_owned(), v.to_owned()))
}

fn check_key(k: &str) -> Result<(), String> {
    if !k.is_empty() && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        Ok(())
    } else {
        Err(format!("invalid option name: {k:?}"))
    }
}

/// `{"scale":50,"keep":true}` -> [(scale,50),(keep,1)]. Only flat string/number/bool values.
fn parse_json_options(s: &str) -> Result<Vec<(String, String)>, String> {
    let Value::Object(map) = serde_json::from_str::<Value>(s)
        .map_err(|e| format!("--json-options is not valid JSON: {e}"))?
    else {
        return Err("--json-options must be a JSON object".into());
    };
    map.into_iter()
        .map(|(k, v)| {
            check_key(&k)?;
            let v = match v {
                Value::String(s) => s,
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => if b { "1" } else { "0" }.to_owned(),
                _ => {
                    return Err(format!(
                        "option {k}: value must be a string, number or boolean"
                    ))
                }
            };
            Ok((k, v))
        })
        .collect()
}

fn absolute(p: &str, cwd: &std::path::Path) -> Result<String, String> {
    if p.is_empty() || p.contains('\0') {
        return Err(format!("invalid path: {p:?}"));
    }
    let path = PathBuf::from(p);
    let abs = if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    };
    abs.into_os_string()
        .into_string()
        .map_err(|_| "paths must be valid UTF-8".to_owned())
}

/// `image.resize` + [(scale,50)] -> `image.resize?scale=50`; values are percent-encoded.
fn build_task(task: &str, options: &[(String, String)]) -> Result<String, String> {
    let (name, query) = task.split_once('?').unwrap_or((task, ""));
    if name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_')
    {
        return Err(format!("invalid task id: {task:?}"));
    }
    if query.bytes().any(|b| b.is_ascii_control()) {
        return Err("invalid task query".into());
    }
    let mut parts: Vec<String> = Some(query)
        .filter(|q| !q.is_empty())
        .map(str::to_owned)
        .into_iter()
        .collect();
    parts.extend(options.iter().map(|(k, v)| format!("{k}={}", encode(v))));
    Ok(if parts.is_empty() {
        name.to_owned()
    } else {
        format!("{name}?{}", parts.join("&"))
    })
}

/// Percent-encode everything but RFC 3986 unreserved bytes (the engine decodes `%XX` as UTF-8).
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn p(args: &[&str]) -> Result<Command, String> {
        parse(
            args.iter().map(OsString::from).collect(),
            Path::new("/work"),
        )
    }

    #[test]
    fn run_builds_task_and_absolute_paths() {
        let Ok(Command::Run(r)) = p(&[
            "run",
            "image.resize",
            "-o",
            "scale=50",
            "--option=w=a b&c",
            "x.png",
            "/abs/y.png",
        ]) else {
            panic!()
        };
        assert_eq!(r.task, "image.resize?scale=50&w=a%20b%26c");
        assert_eq!(r.paths, ["/work/x.png", "/abs/y.png"]);
        assert!(!r.json);
    }

    #[test]
    fn json_options_flat_values_only() {
        let Ok(Command::Run(r)) = p(&[
            "run",
            "t.x",
            "--json-options",
            r#"{"to":"日","keep":true,"n":1.5}"#,
            "--json",
            "a",
        ]) else {
            panic!()
        };
        assert_eq!(r.task, "t.x?to=%E6%97%A5&keep=1&n=1.5");
        assert!(r.json);
        assert!(p(&["run", "t", "--json-options", r#"{"a":[1]}"#, "a"]).is_err());
        assert!(p(&["run", "t", "--json-options", "[]", "a"]).is_err());
        assert!(p(&["run", "t", "-o", "bad key=1", "a"]).is_err());
    }

    #[test]
    fn dashdash_allows_dash_paths() {
        let Ok(Command::Run(r)) = p(&["run", "t", "--", "-weird.txt"]) else {
            panic!()
        };
        assert_eq!(r.paths, ["/work/-weird.txt"]);
    }

    #[test]
    fn preset_takes_only_paths() {
        let Ok(Command::Run(r)) = p(&["run", "--preset", "SNS 1080px / 2MB", "a.png", "/b.png"])
        else {
            panic!()
        };
        assert_eq!(r.preset.as_deref(), Some("SNS 1080px / 2MB"));
        assert_eq!(r.paths, ["/work/a.png", "/b.png"]);
        assert!(r.task.is_empty());
        let Ok(Command::Run(r)) = p(&["run", "--json", "--preset=web", "--", "-x.png"]) else {
            panic!()
        };
        assert_eq!((r.preset.as_deref(), r.json), (Some("web"), true));
        assert_eq!(r.paths, ["/work/-x.png"]);
        assert!(p(&["run", "--preset"]).is_err());
        assert!(p(&["run", "--preset=", "a"]).is_err());
        assert!(p(&["run", "--preset", "web"]).is_err());
        assert!(p(&["run", "--preset", "web", "-o", "scale=5", "a"]).is_err());
    }

    #[test]
    fn apply_and_permanent_flags() {
        let Ok(Command::Run(r)) = p(&["run", "folder.removeEmpty", "--apply", "--permanent", "d"])
        else {
            panic!()
        };
        assert_eq!((r.apply, r.permanent), (true, true));
        let Ok(Command::Run(r)) = p(&["run", "folder.removeEmpty", "d"]) else {
            panic!()
        };
        assert_eq!((r.apply, r.permanent), (false, false));
    }

    #[test]
    fn usage_errors() {
        assert!(p(&[]).is_err());
        assert!(p(&["nope"]).is_err());
        assert!(p(&["run"]).is_err());
        assert!(p(&["run", "task.only"]).is_err());
        assert!(p(&["run", "bad/task", "a"]).is_err());
        assert!(p(&["run", "t", "--bogus", "a"]).is_err());
        assert!(p(&["settings"]).is_err());
        assert_eq!(p(&["settings", "path"]), Ok(Command::SettingsPath));
        assert_eq!(p(&["list", "--json"]), Ok(Command::List { json: true }));
    }
}
