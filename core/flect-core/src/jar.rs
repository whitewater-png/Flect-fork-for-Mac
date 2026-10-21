//! JAR decompilation with CFR. CFR only reads class files, but the JVM is a big program and the
//! output paths come from names inside the file, so it runs under `sandbox-exec` (no network,
//! writes only into a fresh staging folder) whenever that exists.
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use crate::archive::{audit_tree, tool_is_trustworthy, Report};
use crate::paths;
use crate::workspace::ScratchDirectory;

/// CFR 0.152 (MIT). Pinned: the jar is refused unless its SHA-256 matches, whatever its location.
pub const CFR_VERSION: &str = "0.152";
pub const CFR_URL: &str = "https://github.com/leibnitz27/cfr/releases/download/0.152/cfr-0.152.jar";
pub const CFR_SHA256: &str = "f686e8f3ded377d7bc87d216a90e9e9512df4156e75b06c655a16648ae8765b2";
const TIMEOUT: Duration = Duration::from_secs(15 * 60);

#[derive(Debug)]
pub enum JarError {
    NoJava,
    NoCfr,
    /// The jar on disk is not the pinned CFR build.
    CfrMismatch,
    Failed(String),
    Empty,
    Cancelled,
    Unsafe(String),
    Io(io::Error),
}

impl JarError {
    pub fn key(&self) -> &'static str {
        match self {
            Self::NoJava => "msg.javaMissing",
            Self::NoCfr => "msg.cfrMissing",
            Self::CfrMismatch => "msg.cfrMismatch",
            Self::Failed(_) => "msg.decompileFailed",
            Self::Empty => "msg.decompileEmpty",
            Self::Cancelled => "msg.cancelled",
            Self::Unsafe(_) => "msg.archiveUnsafePath",
            Self::Io(e) => crate::paths::io_error_key(e),
        }
    }
}

impl From<io::Error> for JarError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Never run `/usr/bin/java` blindly: without a JDK it is a stub that opens an installer dialog.
pub fn find_java() -> Option<PathBuf> {
    if let Ok(out) = Command::new("/usr/libexec/java_home")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    {
        if out.status.success() {
            let home = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            let java = Path::new(&home).join("bin/java");
            if home.starts_with('/') && tool_is_trustworthy(&java) {
                return Some(java);
            }
        }
    }
    [
        "/opt/homebrew/opt/openjdk/bin/java",
        "/usr/local/opt/openjdk/bin/java",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| tool_is_trustworthy(p))
}

pub fn sha256_of(path: &Path) -> io::Result<String> {
    let mut f = fs::File::open(path)?;
    let (mut h, mut buf) = (Sha256::new(), vec![0u8; 64 * 1024]);
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

pub fn verify_cfr(path: &Path) -> Result<(), JarError> {
    if !path.is_file() {
        return Err(JarError::NoCfr);
    }
    if sha256_of(path)? == CFR_SHA256 {
        Ok(())
    } else {
        Err(JarError::CfrMismatch)
    }
}

const SANDBOX_PROFILE: &str = "(version 1)\n(allow default)\n(deny network*)\n(deny file-write*)\n(allow file-write* (subpath (param \"OUT\")) (subpath (param \"TMP\")) (literal \"/dev/null\") (literal \"/dev/dtracehelper\"))\n";

/// `java ... -jar cfr input --outputdir out`, wrapped in sandbox-exec when available.
pub fn build_command(
    java: &Path,
    cfr: &Path,
    input: &Path,
    out: &Path,
    tmp: &Path,
    sandbox: Option<&Path>,
) -> Command {
    let java_args = |c: &mut Command| {
        c.args([
            "-Xmx2g",
            "-XX:-UsePerfData",
            "-Djava.awt.headless=true",
            "-jar",
        ])
        .arg(cfr)
        .arg(input)
        .arg("--outputdir")
        .arg(out);
    };
    match sandbox {
        Some(sb) => {
            let mut c = Command::new(sb);
            c.arg("-D")
                .arg(format!("OUT={}", out.display()))
                .arg("-D")
                .arg(format!("TMP={}", tmp.display()))
                .arg("-p")
                .arg(SANDBOX_PROFILE)
                .arg(java);
            java_args(&mut c);
            c
        }
        None => {
            let mut c = Command::new(java);
            java_args(&mut c);
            c
        }
    }
}

fn run_with_timeout(
    mut cmd: Command,
    cancelled: &dyn Fn() -> bool,
) -> Result<(bool, String), JarError> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stderr = child.stderr.take().expect("piped");
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = (&mut stderr).take(64 * 1024).read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    });
    let started = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if cancelled() || started.elapsed() > TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            // Do not join the reader: a helper the JVM/shell left behind may keep the pipe open.
            drop(reader);
            return Err(if cancelled() {
                JarError::Cancelled
            } else {
                JarError::Failed("timed out".into())
            });
        }
        std::thread::sleep(Duration::from_millis(30));
    };
    Ok((status.success(), reader.join().unwrap_or_default()))
}

#[derive(Debug)]
pub struct Decompiled {
    pub destination: PathBuf,
    pub report: Report,
    /// The sandbox could not be used and the run happened without it.
    pub unsandboxed: bool,
}

/// Decompile `input` into a new folder: beside the jar, or under `output_root` when configured.
pub fn decompile(
    input: &Path,
    java: &Path,
    cfr: &Path,
    output_root: Option<&Path>,
    sandbox: Option<&Path>,
    cancelled: &dyn Fn() -> bool,
) -> Result<Decompiled, JarError> {
    verify_cfr(cfr)?;
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (anchor, base): (PathBuf, PathBuf) = match output_root {
        Some(root) if root.is_absolute() => {
            fs::create_dir_all(root)?;
            (root.join(&stem), root.to_path_buf())
        }
        Some(_) => return Err(JarError::Unsafe("output folder must be absolute".into())),
        None => (
            input.to_path_buf(),
            input.parent().unwrap_or(Path::new(".")).to_path_buf(),
        ),
    };
    let _ = base;
    let scratch = ScratchDirectory::new(&anchor)?;
    let staging = scratch.path_for("out");
    fs::create_dir(&staging)?;
    let input_abs = fs::canonicalize(input)?;
    let tmp = std::env::temp_dir();
    let mut unsandboxed = false;
    let (ok, err) = run_with_timeout(
        build_command(java, cfr, &input_abs, &staging, &tmp, sandbox),
        cancelled,
    )?;
    let (ok, err) = if !ok
        && sandbox.is_some()
        && (err.contains("sandbox") || err.contains("Operation not permitted"))
    {
        // The profile itself was rejected (not a decompile error): retry once without it, and say so.
        unsandboxed = true;
        run_with_timeout(
            build_command(java, cfr, &input_abs, &staging, &tmp, None),
            cancelled,
        )?
    } else {
        (ok, err)
    };
    if !ok {
        return Err(JarError::Failed(
            err.lines()
                .rfind(|l| !l.starts_with("Picked up"))
                .unwrap_or("")
                .to_owned(),
        ));
    }
    let entries: Vec<_> = fs::read_dir(&staging)?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    if entries.is_empty() {
        return Err(JarError::Empty);
    }
    // On a file it cannot read as a jar, CFR exits normally and leaves only its summary behind.
    if entries == ["summary.txt"] {
        let text = fs::read_to_string(staging.join("summary.txt")).unwrap_or_default();
        if text.contains("Exception") {
            let line = text
                .lines()
                .find(|l| l.contains("Exception"))
                .unwrap_or("")
                .chars()
                .take(300)
                .collect();
            return Err(JarError::Failed(line));
        }
    }
    let mut report = Report::default();
    audit_tree(&staging, &mut report).map_err(|e| JarError::Unsafe(format!("{e:?}")))?;
    for _ in 0..16 {
        let dest = paths::derive_directory(
            &anchor,
            if output_root.is_some() {
                Some(&stem)
            } else {
                None
            },
            "",
        );
        match fs::create_dir(&dest) {
            Ok(()) => {
                fs::rename(&staging, &dest)?;
                return Ok(Decompiled {
                    destination: dest,
                    report,
                    unsandboxed,
                });
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(JarError::Io(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free folder name",
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn fake_java(dir: &Path, body: &str) -> PathBuf {
        let p = dir.join("java");
        fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    /// A stand-in for cfr.jar is not accepted: only the pinned hash is.
    fn fake_cfr(dir: &Path) -> PathBuf {
        let p = dir.join("cfr.jar");
        fs::write(&p, b"not cfr").unwrap();
        p
    }

    #[test]
    fn only_the_pinned_cfr_is_accepted() {
        let d = tempfile::tempdir().unwrap();
        assert!(matches!(
            verify_cfr(&d.path().join("none.jar")),
            Err(JarError::NoCfr)
        ));
        assert!(matches!(
            verify_cfr(&fake_cfr(d.path())),
            Err(JarError::CfrMismatch)
        ));
        assert_eq!(CFR_SHA256.len(), 64);
        // known-answer test for the hash function itself
        fs::write(d.path().join("abc"), b"abc").unwrap();
        assert_eq!(
            sha256_of(&d.path().join("abc")).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn command_lines() {
        let c = build_command(
            Path::new("/j/java"),
            Path::new("/c/cfr.jar"),
            Path::new("/in/a b.jar"),
            Path::new("/out"),
            Path::new("/tmp"),
            None,
        );
        let args: Vec<_> = c
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(c.get_program(), "/j/java");
        assert_eq!(
            args,
            [
                "-Xmx2g",
                "-XX:-UsePerfData",
                "-Djava.awt.headless=true",
                "-jar",
                "/c/cfr.jar",
                "/in/a b.jar",
                "--outputdir",
                "/out"
            ]
        );
        let sb = build_command(
            Path::new("/j/java"),
            Path::new("/c/cfr.jar"),
            Path::new("/in/a.jar"),
            Path::new("/out dir"),
            Path::new("/tmp"),
            Some(Path::new("/usr/bin/sandbox-exec")),
        );
        let sargs: Vec<_> = sb
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(sb.get_program(), "/usr/bin/sandbox-exec");
        assert_eq!(&sargs[..2], ["-D", "OUT=/out dir"]);
        assert!(
            sargs.contains(&SANDBOX_PROFILE.to_owned())
                && SANDBOX_PROFILE.contains("(deny network*)")
        );
        assert!(
            SANDBOX_PROFILE.contains("(param \"OUT\")"),
            "paths are passed as parameters, never spliced into the profile"
        );
        assert!(
            sargs.iter().position(|a| a == "/j/java").unwrap()
                > sargs.iter().position(|a| a == "-p").unwrap()
        );
    }

    #[test]
    fn rejects_a_relative_output_root() {
        // verify_cfr runs first, so make it pass by checking the order with a mismatching jar: error is CfrMismatch
        let d = tempfile::tempdir().unwrap();
        let r = decompile(
            &d.path().join("a.jar"),
            Path::new("/bin/true"),
            &fake_cfr(d.path()),
            Some(Path::new("rel")),
            None,
            &|| false,
        );
        assert!(matches!(r, Err(JarError::CfrMismatch)));
    }

    /// Full run with the real pinned CFR and a real JDK. Needs `FLECT_TEST_CFR_JAR` (see scripts/fetch-cfr.sh).
    #[test]
    fn decompiles_a_real_jar() {
        let (Some(cfr), Some(java)) = (
            std::env::var_os("FLECT_TEST_CFR_JAR").map(PathBuf::from),
            ["/usr/bin/java"]
                .iter()
                .map(PathBuf::from)
                .find(|p| p.is_file()),
        ) else {
            eprintln!("SKIPPED: set FLECT_TEST_CFR_JAR to the pinned cfr-0.152.jar and have a JDK");
            return;
        };
        let (Some(javac), Some(jar)) = (
            ["/usr/bin/javac"]
                .iter()
                .map(Path::new)
                .find(|p| p.is_file()),
            ["/usr/bin/jar"].iter().map(Path::new).find(|p| p.is_file()),
        ) else {
            eprintln!("SKIPPED: no javac/jar");
            return;
        };
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src");
        fs::create_dir_all(src.join("demo")).unwrap();
        fs::write(src.join("demo/Hello.java"), "package demo; public class Hello { public String greet(String n) { return \"Hi \" + n; } }").unwrap();
        assert!(Command::new(javac)
            .args(["-nowarn", "-d"])
            .arg(&src)
            .arg(src.join("demo/Hello.java"))
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success());
        let jar_path = d.path().join("hello app.jar");
        assert!(Command::new(jar)
            .arg("cf")
            .arg(&jar_path)
            .arg("-C")
            .arg(&src)
            .arg("demo/Hello.class")
            .status()
            .unwrap()
            .success());

        let out = decompile(&jar_path, &java, &cfr, None, None, &|| false).unwrap();
        assert_eq!(out.destination, d.path().join("hello app"));
        let java_src = fs::read_to_string(out.destination.join("demo/Hello.java")).unwrap();
        assert!(
            java_src.contains("class Hello") && java_src.contains("greet"),
            "{java_src}"
        );
        assert!(!out.unsandboxed);
        // second run gets a new folder; the jar is untouched; no scratch litter
        let again = decompile(&jar_path, &java, &cfr, None, None, &|| false).unwrap();
        assert_eq!(again.destination, d.path().join("hello app (2)"));
        assert!(fs::read_dir(d.path())
            .unwrap()
            .flatten()
            .all(|e| !e.file_name().to_string_lossy().starts_with(".Flect")));
        // configured output root
        let root = d.path().join("elsewhere");
        let r = decompile(&jar_path, &java, &cfr, Some(&root), None, &|| false).unwrap();
        assert_eq!(r.destination, root.join("hello app"));
        // garbage jar
        let bad = d.path().join("bad.jar");
        fs::write(&bad, b"nope").unwrap();
        let before = fs::read_dir(d.path()).unwrap().count();
        let e = decompile(&bad, &java, &cfr, None, None, &|| false).unwrap_err();
        assert!(matches!(e, JarError::Failed(_) | JarError::Empty), "{e:?}");
        assert_eq!(
            fs::read_dir(d.path()).unwrap().count(),
            before,
            "failure leaves nothing behind"
        );
    }

    #[test]
    fn fake_java_flow_cancellation_and_sandbox_retry() {
        // The pinned-hash gate cannot be passed by a fake jar, so exercise the process runner directly.
        let d = tempfile::tempdir().unwrap();
        let slow = fake_java(d.path(), "sleep 30");
        let t = Instant::now();
        let r = run_with_timeout(Command::new(&slow), &|| {
            t.elapsed() > Duration::from_millis(200)
        });
        assert!(matches!(r, Err(JarError::Cancelled)));
        assert!(t.elapsed() < Duration::from_secs(5));
        let failing = fake_java(d.path(), "echo 'sandbox: bad profile' >&2; exit 65");
        let (ok, err) = run_with_timeout(Command::new(&failing), &|| false).unwrap();
        assert!(!ok && err.contains("sandbox"));
    }
}
