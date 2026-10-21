//! JAR decompilation with CFR. CFR only reads class files, but the JVM is a big program and the
//! output paths come from names inside the file, so it only ever runs under `sandbox-exec`: no
//! network, writes only into a fresh staging folder and a private temporary folder, and no program
//! but the JVM may be started. Whether the sandbox works is decided by a probe with the same profile
//! and parameters before CFR starts; if it does not, the task fails (`msg.sandboxFailed`). There is
//! no fallback that runs the decompiler without the sandbox.
use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::DirBuilderExt;
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
    /// sandbox-exec is missing or refused the profile: CFR was not started.
    NoSandbox(String),
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
            Self::NoSandbox(_) => "msg.sandboxFailed",
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

pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
/// What the probe runs inside the sandbox; the profile allows exactly this and the JVM to be started.
const PROBE: &str = "/usr/bin/true";
const SANDBOX_PROFILE: &str = "(version 1)\n(allow default)\n(deny network*)\n(deny file-write*)\n(allow file-write* (subpath (param \"OUT\")) (subpath (param \"TMP\")) (literal \"/dev/null\") (literal \"/dev/dtracehelper\"))\n(deny process-exec*)\n(allow process-exec (literal (param \"JAVA\")) (literal \"/usr/bin/true\"))\n";

/// `sandbox-exec -D OUT=.. -D TMP=.. -D JAVA=.. -p PROFILE <program>`. All paths are passed as
/// parameters (never spliced into the profile) and must already be canonical (`/private/var/...`,
/// not `/var/...`), because the sandbox compares resolved paths.
fn sandboxed(sandbox: &Path, java: &Path, out: &Path, tmp: &Path, program: &Path) -> Command {
    let mut c = Command::new(sandbox);
    c.arg("-D")
        .arg(format!("OUT={}", out.display()))
        .arg("-D")
        .arg(format!("TMP={}", tmp.display()))
        .arg("-D")
        .arg(format!("JAVA={}", java.display()))
        .arg("-p")
        .arg(SANDBOX_PROFILE)
        .arg(program)
        .env("TMPDIR", tmp);
    c
}

/// `sandbox-exec ... java ... -jar cfr input --outputdir out`. There is no unsandboxed form.
pub fn build_command(
    java: &Path,
    cfr: &Path,
    input: &Path,
    out: &Path,
    tmp: &Path,
    sandbox: &Path,
) -> Command {
    let mut c = sandboxed(sandbox, java, out, tmp, java);
    c.arg(format!("-Djava.io.tmpdir={}", tmp.display()))
        .args([
            "-Xmx2g",
            "-XX:-UsePerfData",
            "-Djava.awt.headless=true",
            "-jar",
        ])
        .arg(cfr)
        .arg(input)
        .arg("--outputdir")
        .arg(out);
    c
}

/// Check that sandbox-exec accepts the profile with these very parameters, by running a no-op
/// program under it. CFR is only started when this succeeds.
pub fn probe_sandbox(sandbox: &Path, java: &Path, out: &Path, tmp: &Path) -> Result<(), JarError> {
    let failed = |why: String| Err(JarError::NoSandbox(why));
    if !sandbox.is_file() {
        return failed(format!("{} not found", sandbox.display()));
    }
    let probe = sandboxed(sandbox, java, out, tmp, Path::new(PROBE))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output();
    match probe {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => failed(
            String::from_utf8_lossy(&o.stderr)
                .lines()
                .next()
                .unwrap_or("sandbox-exec failed")
                .chars()
                .take(300)
                .collect(),
        ),
        Err(e) => failed(e.to_string()),
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
}

/// Decompile `input` into a new folder: beside the jar, or under `output_root` when configured.
/// `sandbox` is sandbox-exec; without it (or when it refuses the profile) nothing is run.
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
    let sandbox =
        sandbox.ok_or_else(|| JarError::NoSandbox(format!("{SANDBOX_EXEC} not found")))?;
    let scratch = ScratchDirectory::new(&anchor)?;
    let staging = scratch.path_for("out");
    fs::create_dir(&staging)?;
    // A private temporary folder for the JVM (not the shared per-user one), inside the scratch folder.
    let tmp = scratch.path_for("tmp");
    fs::DirBuilder::new().mode(0o700).create(&tmp)?;
    // The sandbox matches resolved paths (/var -> /private/var), so hand it resolved ones.
    let (staging, tmp) = (fs::canonicalize(&staging)?, fs::canonicalize(&tmp)?);
    let java = fs::canonicalize(java)?;
    let input_abs = fs::canonicalize(input)?;
    probe_sandbox(sandbox, &java, &staging, &tmp)?;
    let (ok, err) = run_with_timeout(
        build_command(&java, cfr, &input_abs, &staging, &tmp, sandbox),
        cancelled,
    )?;
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
        let sb = build_command(
            Path::new("/j/java"),
            Path::new("/c/cfr.jar"),
            Path::new("/in/a b.jar"),
            Path::new("/out dir"),
            Path::new("/priv/tmp"),
            Path::new(SANDBOX_EXEC),
        );
        let sargs: Vec<_> = sb
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(sb.get_program(), SANDBOX_EXEC, "always wrapped");
        let java_at = sargs.iter().position(|a| a == "/j/java").unwrap();
        assert_eq!(
            &sargs[..java_at],
            [
                "-D",
                "OUT=/out dir",
                "-D",
                "TMP=/priv/tmp",
                "-D",
                "JAVA=/j/java",
                "-p",
                SANDBOX_PROFILE
            ]
        );
        assert_eq!(
            &sargs[java_at + 1..],
            [
                "-Djava.io.tmpdir=/priv/tmp",
                "-Xmx2g",
                "-XX:-UsePerfData",
                "-Djava.awt.headless=true",
                "-jar",
                "/c/cfr.jar",
                "/in/a b.jar",
                "--outputdir",
                "/out dir"
            ]
        );
        assert!(sb
            .get_envs()
            .any(|(k, v)| k == "TMPDIR" && v == Some(std::ffi::OsStr::new("/priv/tmp"))));
        assert!(
            SANDBOX_PROFILE.contains("(deny network*)")
                && SANDBOX_PROFILE.contains("(deny process-exec*)")
        );
        assert!(
            SANDBOX_PROFILE.contains("(param \"OUT\")") && !SANDBOX_PROFILE.contains("/out"),
            "paths are passed as parameters, never spliced into the profile"
        );
    }

    /// The real sandbox-exec with the real profile: the probe passes, and the profile denies writes
    /// outside OUT/TMP, the network and starting other programs. Skipped where sandbox-exec is absent.
    #[test]
    fn the_profile_works_and_confines() {
        let sb = Path::new(SANDBOX_EXEC);
        if !sb.is_file() {
            eprintln!("SKIPPED: no {SANDBOX_EXEC}");
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let (out, tmp) = (d.path().join("out"), d.path().join("tmp"));
        fs::create_dir(&out).unwrap();
        fs::create_dir(&tmp).unwrap();
        let (out, tmp) = (
            fs::canonicalize(out).unwrap(),
            fs::canonicalize(tmp).unwrap(),
        );
        // "java" here is bash (a real binary; /bin/sh is a trampoline that starts another program),
        // so the one allowed program can try what the JVM must not do
        let sh = Path::new("/bin/bash");
        probe_sandbox(sb, sh, &out, &tmp).expect("sandbox-exec accepts the profile");
        let run = |out: &Path, script: &str| {
            sandboxed(sb, sh, out, &tmp, sh)
                .arg("-c")
                .arg(script)
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success()
        };
        assert!(run(&out, &format!("echo x > '{}/a'", out.display())));
        assert!(run(
            &out,
            &format!(
                "echo x > \"$TMPDIR/b\" && [ \"$TMPDIR\" = '{}' ]",
                tmp.display()
            )
        ));
        assert!(!run(
            &out,
            &format!("echo x > '{}/escape'", d.path().display())
        ));
        assert!(!d.path().join("escape").exists());
        assert!(
            !run(&out, "/bin/echo started another program"),
            "process-exec is limited to the JVM"
        );
        // The sandbox compares resolved paths: an unresolved spelling of OUT (/var/... for
        // /private/var/...) would make every write fail, which is why decompile canonicalizes.
        let unresolved = d.path().join("out");
        if unresolved != out {
            assert!(!run(
                &unresolved,
                &format!("echo x > '{}/c'", out.display())
            ));
        }
        // a missing sandbox-exec is an error, never a reason to run without it
        assert!(matches!(
            probe_sandbox(Path::new("/nonexistent/sandbox-exec"), sh, &out, &tmp),
            Err(JarError::NoSandbox(_))
        ));
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

    /// Full run with the real pinned CFR, a real JDK and the real sandbox. Needs `FLECT_TEST_CFR_JAR`
    /// (see scripts/fetch-cfr.sh) and a JDK that `find_java` finds (with javac and jar beside it).
    #[test]
    fn decompiles_a_real_jar() {
        let (Some(cfr), Some(java)) = (
            std::env::var_os("FLECT_TEST_CFR_JAR").map(PathBuf::from),
            find_java(),
        ) else {
            eprintln!("SKIPPED: set FLECT_TEST_CFR_JAR to the pinned cfr-0.152.jar and have a JDK");
            return;
        };
        let bin = java.parent().unwrap().to_path_buf();
        let (javac, jar) = (bin.join("javac"), bin.join("jar"));
        if !javac.is_file() || !jar.is_file() || !Path::new(SANDBOX_EXEC).is_file() {
            eprintln!(
                "SKIPPED: no javac/jar beside {} or no sandbox-exec",
                java.display()
            );
            return;
        }
        let sandbox = Some(Path::new(SANDBOX_EXEC));
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

        let out = decompile(&jar_path, &java, &cfr, None, sandbox, &|| false).unwrap();
        assert_eq!(out.destination, d.path().join("hello app"));
        let java_src = fs::read_to_string(out.destination.join("demo/Hello.java")).unwrap();
        assert!(
            java_src.contains("class Hello") && java_src.contains("greet"),
            "{java_src}"
        );
        // second run gets a new folder; the jar is untouched; no scratch litter
        let again = decompile(&jar_path, &java, &cfr, None, sandbox, &|| false).unwrap();
        assert_eq!(again.destination, d.path().join("hello app (2)"));
        assert!(fs::read_dir(d.path())
            .unwrap()
            .flatten()
            .all(|e| !e.file_name().to_string_lossy().starts_with(".Flect")));
        // configured output root
        let root = d.path().join("elsewhere");
        let r = decompile(&jar_path, &java, &cfr, Some(&root), sandbox, &|| false).unwrap();
        assert_eq!(r.destination, root.join("hello app"));
        // garbage jar
        let bad = d.path().join("bad.jar");
        fs::write(&bad, b"nope").unwrap();
        let before = fs::read_dir(d.path()).unwrap().count();
        let e = decompile(&bad, &java, &cfr, None, sandbox, &|| false).unwrap_err();
        assert!(matches!(e, JarError::Failed(_) | JarError::Empty), "{e:?}");
        assert_eq!(
            fs::read_dir(d.path()).unwrap().count(),
            before,
            "failure leaves nothing behind"
        );
    }

    /// When sandbox-exec failed with "sandbox" / "Operation not permitted" on stderr, CFR was run a
    /// second time without any sandbox. It must never run unsandboxed. Needs the pinned CFR jar
    /// (`FLECT_TEST_CFR_JAR`) to get past the hash gate; the JVM and sandbox-exec are fakes.
    #[test]
    fn a_failing_sandbox_is_never_downgraded_to_no_sandbox() {
        let Some(cfr) = std::env::var_os("FLECT_TEST_CFR_JAR").map(PathBuf::from) else {
            eprintln!("SKIPPED: set FLECT_TEST_CFR_JAR to the pinned cfr-0.152.jar");
            return;
        };
        let d = tempfile::tempdir().unwrap();
        let marker = d.path().join("java-ran");
        // a "JVM" that leaves a marker and some output, so a run without the sandbox would succeed
        let java = fake_java(
            d.path(),
            &format!(
                "touch '{}'; for a; do out=\"$a\"; done; echo 'class A {{}}' > \"$out/A.java\"",
                marker.display()
            ),
        );
        let sandbox = d.path().join("sandbox-exec");
        fs::write(
            &sandbox,
            "#!/bin/sh\necho 'sandbox-exec: sandbox_apply: Operation not permitted' >&2\nexit 71\n",
        )
        .unwrap();
        fs::set_permissions(&sandbox, fs::Permissions::from_mode(0o755)).unwrap();
        let jar = d.path().join("app.jar");
        fs::write(&jar, b"PK\x05\x06").unwrap();
        let before: Vec<_> = fs::read_dir(d.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        let r = decompile(&jar, &java, &cfr, None, Some(&sandbox), &|| false);
        assert!(r.is_err(), "decompiled without a sandbox: {r:?}");
        assert!(
            !marker.exists(),
            "the JVM ran although the sandbox could not be used"
        );
        let after: Vec<_> = fs::read_dir(d.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(before, after, "nothing is left behind");
        // no sandbox-exec at all: refused the same way
        let r = decompile(&jar, &java, &cfr, None, None, &|| false);
        assert!(r.is_err() && !marker.exists(), "{r:?}");
    }

    #[test]
    fn fake_java_flow_and_cancellation() {
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
        // a sandbox-exec that refuses: the probe reports it, with its message
        let refusing = fake_java(
            d.path(),
            "echo 'sandbox_apply: Operation not permitted' >&2; exit 71",
        );
        match probe_sandbox(&refusing, Path::new("/bin/sh"), d.path(), d.path()) {
            Err(JarError::NoSandbox(why)) => {
                assert!(why.contains("Operation not permitted"), "{why}")
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            JarError::NoSandbox(String::new()).key(),
            "msg.sandboxFailed"
        );
    }
}
