//! make-workflows: generate the Finder Quick Actions (`.workflow` bundles) from the menu catalog.
//!
//!   make-workflows [--out DIR] [--app NAME]      (default DIR: ~/Library/Services)
//!
//! Each action only stages the selected paths in a private temp file and opens the app with
//! `--task <id> --input <file>`; every choice (preset, format, ...) is made inside the app.
use std::fs;
use std::path::{Path, PathBuf};

use flect_core::menu;

/// Where an action appears: UTIs of the items it accepts (matched by conformance).
struct Scope {
    label: &'static str,
    utis: &'static [&'static str],
}

fn scope(category: &str) -> Option<Scope> {
    // UTI names for formats macOS does not declare itself (mkv, webm, ...) are best effort:
    // a file whose type is unknown to the system gets a dynamic UTI and will not match.
    let (label, utis): (&'static str, &'static [&'static str]) = match category {
        "folder" => ("Folder", &["public.folder"]),
        "archive" => (
            "Archive",
            &[
                "public.zip-archive",
                "public.tar-archive",
                "org.gnu.gnu-zip-tar-archive",
                "org.7-zip.7-zip-archive",
                "com.rarlab.rar-archive",
            ],
        ),
        "pdf" => ("PDF", &["com.adobe.pdf"]),
        "image" => (
            "Image",
            &[
                "public.png",
                "public.jpeg",
                "org.webmproject.webp",
                "public.heic",
                "public.heif",
                "com.microsoft.ico",
                "com.compuserve.gif",
                "public.svg-image",
            ],
        ),
        "video" => (
            "Video",
            &[
                "public.movie",
                "public.mpeg-4",
                "com.apple.quicktime-movie",
                "public.avi",
                "org.matroska.mkv",
                // macOS does not declare .mkv itself; without a third-party handler Finder reports this
                // extension-derived dynamic type (deterministic for "public.filename-extension" = "mkv").
                "dyn.ah62d4rv4ge804450",
                "org.webmproject.webm",
                "com.adobe.flash.video",
            ],
        ),
        "audio" => (
            "Audio",
            &[
                "public.audio",
                "public.mp3",
                "com.microsoft.waveform-audio",
                "public.aiff-audio",
                "public.aac-audio",
                "org.xiph.ogg-audio",
                "com.microsoft.windows-media-wma",
            ],
        ),
        "text" => (
            "Text",
            &[
                "public.text",
                "public.json",
                "public.xml",
                "public.comma-separated-values-text",
            ],
        ),
        "jar" => ("JAR", &["com.sun.java-archive"]),
        _ => return None,
    };
    Some(Scope { label, utis })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub title: String,
    pub task: String,
    pub utis: Vec<String>,
}

/// One action per second-level menu entry (a group such as `image.resize` opens its chooser in the app).
pub fn actions() -> Vec<Action> {
    let mut out = Vec::new();
    for category in menu::catalog() {
        let Some(scope) = scope(&category.id) else {
            continue;
        };
        for entry in &category.children {
            out.push(Action {
                title: format!("Flect: {} {}", scope.label, entry.label),
                task: entry.id.clone(),
                utis: scope.utis.iter().map(|s| (*s).to_owned()).collect(),
            });
        }
    }
    out
}

fn valid_token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ' '))
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Stable id per action so regenerating does not churn the bundles.
fn uuid_for(seed: &str, salt: u64) -> String {
    let mut h: u128 = 0x6c62272e07bb014262b821756295c58d ^ salt as u128;
    for b in seed.bytes() {
        h ^= b as u128;
        h = h.wrapping_mul(0x0000000001000000000000000000013B);
    }
    let x = format!("{h:032X}");
    format!(
        "{}-{}-{}-{}-{}",
        &x[0..8],
        &x[8..12],
        &x[12..16],
        &x[16..20],
        &x[20..32]
    )
}

pub fn script_for(task: &str, app: &str) -> String {
    // task/app are validated tokens, so quoting them is enough.
    format!(
        "f=\"$(mktemp -t flect-sel)\" || exit 1\nchmod 600 \"$f\"\nfor p in \"$@\"; do printf '%s\\n' \"$p\" >> \"$f\"; done\nopen -n -a \"{app}\" --args --task \"{task}\" --input \"$f\"\n"
    )
}

pub fn info_plist(a: &Action) -> String {
    let utis: String = a
        .utis
        .iter()
        .map(|u| format!("      <string>{}</string>\n", xml(u)))
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>NSServices</key>
  <array>
    <dict>
      <key>NSBackgroundColorName</key><string>background</string>
      <key>NSIconName</key><string>NSActionTemplate</string>
      <key>NSMenuItem</key><dict><key>default</key><string>{title}</string></dict>
      <key>NSMessage</key><string>runWorkflowAsService</string>
      <key>NSRequiredContext</key><dict><key>NSApplicationIdentifier</key><string>com.apple.finder</string></dict>
      <key>NSSendFileTypes</key>
      <array>
{utis}      </array>
    </dict>
  </array>
</dict>
</plist>
"#,
        title = xml(&a.title)
    )
}

pub fn document_wflow(a: &Action, app: &str) -> String {
    let script = xml(&script_for(&a.task, app));
    let (i, o, u) = (
        uuid_for(&a.task, 1),
        uuid_for(&a.task, 2),
        uuid_for(&a.task, 3),
    );
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>AMApplicationBuild</key><string>523</string>
  <key>AMApplicationVersion</key><string>2.10</string>
  <key>AMDocumentVersion</key><string>2</string>
  <key>actions</key>
  <array>
    <dict>
      <key>action</key>
      <dict>
        <key>AMAccepts</key><dict><key>Container</key><string>List</string><key>Optional</key><true/><key>Types</key><array><string>com.apple.cocoa.string</string></array></dict>
        <key>AMActionVersion</key><string>2.0.3</string>
        <key>AMApplication</key><array><string>Automator</string></array>
        <key>AMParameterProperties</key>
        <dict>
          <key>COMMAND_STRING</key><dict/>
          <key>CheckedForUserDefaultShell</key><dict/>
          <key>inputMethod</key><dict/>
          <key>shell</key><dict/>
          <key>source</key><dict/>
        </dict>
        <key>AMProvides</key><dict><key>Container</key><string>List</string><key>Types</key><array><string>com.apple.cocoa.string</string></array></dict>
        <key>ActionBundlePath</key><string>/System/Library/Automator/Run Shell Script.action</string>
        <key>ActionName</key><string>Run Shell Script</string>
        <key>ActionParameters</key>
        <dict>
          <key>COMMAND_STRING</key><string>{script}</string>
          <key>CheckedForUserDefaultShell</key><true/>
          <key>inputMethod</key><integer>1</integer>
          <key>shell</key><string>/bin/bash</string>
          <key>source</key><string></string>
        </dict>
        <key>BundleIdentifier</key><string>com.apple.RunShellScript</string>
        <key>CFBundleVersion</key><string>2.0.3</string>
        <key>CanShowSelectedItemsWhenRun</key><false/>
        <key>CanShowWhenRun</key><true/>
        <key>Category</key><array><string>AMCategoryUtilities</string></array>
        <key>Class Name</key><string>RunShellScriptAction</string>
        <key>InputUUID</key><string>{i}</string>
        <key>OutputUUID</key><string>{o}</string>
        <key>UUID</key><string>{u}</string>
        <key>UnlocalizedApplications</key><array><string>Automator</string></array>
        <key>arguments</key><dict/>
        <key>isViewVisible</key><integer>1</integer>
      </dict>
    </dict>
  </array>
  <key>connectors</key><dict/>
  <key>workflowMetaData</key>
  <dict>
    <key>serviceApplicationBundleID</key><string>com.apple.finder</string>
    <key>serviceInputTypeIdentifier</key><string>com.apple.Automator.fileSystemObject</string>
    <key>serviceOutputTypeIdentifier</key><string>com.apple.Automator.nothing</string>
    <key>serviceProcessesInput</key><integer>0</integer>
    <key>workflowTypeIdentifier</key><string>com.apple.Automator.servicesMenu</string>
  </dict>
</dict>
</plist>
"#
    )
}

pub fn write_all(out: &Path, app: &str) -> std::io::Result<Vec<PathBuf>> {
    assert!(valid_token(app), "unsafe app name");
    let mut written = Vec::new();
    for a in actions() {
        assert!(valid_token(&a.task), "unsafe task id {}", a.task);
        let bundle = out.join(format!("{}.workflow", a.title.replace(':', "")));
        fs::create_dir_all(bundle.join("Contents"))?;
        fs::write(bundle.join("Contents/Info.plist"), info_plist(&a))?;
        fs::write(
            bundle.join("Contents/document.wflow"),
            document_wflow(&a, app),
        )?;
        written.push(bundle);
    }
    Ok(written)
}

fn main() {
    let mut out: Option<PathBuf> = None;
    let mut app = "Flect-for-Mac".to_owned();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().map(PathBuf::from),
            "--app" => app = args.next().unwrap_or(app),
            _ => {
                eprintln!("usage: make-workflows [--out DIR] [--app NAME]");
                std::process::exit(2);
            }
        }
    }
    if !valid_token(&app) {
        eprintln!("--app may contain only letters, digits, space . _ -");
        std::process::exit(2);
    }
    let out = out
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Services")))
        .expect("no output directory");
    match write_all(&out, &app) {
        Ok(list) => println!("wrote {} Quick Actions to {}", list.len(), out.display()),
        Err(e) => {
            eprintln!("failed: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plist::Value;

    #[test]
    fn one_action_per_second_level_entry_with_unique_titles() {
        let acts = actions();
        assert_eq!(acts.len(), 35);
        let mut titles: Vec<_> = acts.iter().map(|a| a.title.clone()).collect();
        titles.sort();
        titles.dedup();
        assert_eq!(titles.len(), acts.len());
        assert!(acts
            .iter()
            .any(|a| a.title == "Flect: Image Resize" && a.task == "image.resize"));
        assert!(acts.iter().any(|a| a.task == "image.ocr"));
        assert!(acts
            .iter()
            .all(|a| valid_token(&a.task) && !a.utis.is_empty()));
    }

    #[test]
    fn video_actions_also_match_mkv_where_macos_gives_it_a_dynamic_type() {
        let a = actions()
            .into_iter()
            .find(|a| a.task == "video.trim")
            .unwrap();
        assert!(a.utis.iter().any(|u| u == "org.matroska.mkv"));
        assert!(a.utis.iter().any(|u| u == "dyn.ah62d4rv4ge804450"));
    }

    #[test]
    fn generated_plists_parse_and_carry_the_essentials() {
        let a = actions()
            .into_iter()
            .find(|a| a.task == "text.convert")
            .unwrap();
        let info = Value::from_reader_xml(info_plist(&a).as_bytes()).unwrap();
        let svc = info.as_dictionary().unwrap()["NSServices"]
            .as_array()
            .unwrap()[0]
            .as_dictionary()
            .unwrap();
        assert_eq!(
            svc["NSMenuItem"].as_dictionary().unwrap()["default"].as_string(),
            Some("Flect: Text Convert")
        );
        assert_eq!(svc["NSMessage"].as_string(), Some("runWorkflowAsService"));
        assert!(svc["NSSendFileTypes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_string() == Some("public.text")));

        let wf = Value::from_reader_xml(document_wflow(&a, "Flect-for-Mac").as_bytes()).unwrap();
        let d = wf.as_dictionary().unwrap();
        let action = d["actions"].as_array().unwrap()[0].as_dictionary().unwrap()["action"]
            .as_dictionary()
            .unwrap();
        let params = action["ActionParameters"].as_dictionary().unwrap();
        // "as arguments" - the setting whose absence made the PoC invisible/empty
        assert_eq!(params["inputMethod"].as_signed_integer(), Some(1));
        assert_eq!(params["shell"].as_string(), Some("/bin/bash"));
        let script = params["COMMAND_STRING"].as_string().unwrap();
        assert!(
            script.contains("--task \"text.convert\"")
                && script.contains("open -n -a \"Flect-for-Mac\"")
        );
        assert!(script.contains(">> \"$f\"") && script.contains("chmod 600"));
        let meta = d["workflowMetaData"].as_dictionary().unwrap();
        assert_eq!(
            meta["workflowTypeIdentifier"].as_string(),
            Some("com.apple.Automator.servicesMenu")
        );
        assert_eq!(
            meta["serviceApplicationBundleID"].as_string(),
            Some("com.apple.finder")
        );
    }

    #[test]
    fn uuids_are_stable_distinct_and_well_formed() {
        assert_eq!(uuid_for("a", 1), uuid_for("a", 1));
        assert_ne!(uuid_for("a", 1), uuid_for("a", 2));
        assert_ne!(uuid_for("a", 1), uuid_for("b", 1));
        let u = uuid_for("image.ocr", 1);
        assert_eq!(u.len(), 36);
        assert_eq!(u.matches('-').count(), 4);
    }

    #[test]
    fn tokens_reject_shell_metacharacters() {
        for bad in [
            "", "a\"b", "a$b", "a`b`", "a;b", "a\nb", "a'b", "a\\b", "a&b",
        ] {
            assert!(!valid_token(bad), "{bad:?}");
        }
        assert!(valid_token("Flect-for-Mac") && valid_token("image.resize.p1"));
    }

    #[test]
    fn writes_bundles_to_disk() {
        let d = tempfile::tempdir().unwrap();
        let list = write_all(d.path(), "Flect-for-Mac").unwrap();
        assert_eq!(list.len(), 35);
        assert!(d
            .path()
            .join("Flect Image Resize.workflow/Contents/document.wflow")
            .is_file());
        for b in &list {
            for f in ["Info.plist", "document.wflow"] {
                Value::from_file(b.join("Contents").join(f)).unwrap();
            }
        }
    }
}
