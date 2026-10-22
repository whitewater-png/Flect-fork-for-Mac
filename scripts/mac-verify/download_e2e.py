#!/usr/bin/env python3
"""End-to-end check of the web download through the REAL flect-engine, a real yt-dlp and a real ffmpeg.

It serves a short clip it makes itself from a local web server (127.0.0.1), so no third-party site is contacted
and nothing that is not yours is downloaded. The engine looks for yt-dlp / ffmpeg / ffprobe only in
/opt/homebrew/bin and /usr/local/bin (as the app does), which is part of what this checks.

    python3 scripts/mac-verify/download_e2e.py [path/to/flect-engine]

Without an argument it looks for the engine in ~/Applications/Flect-for-Mac.app, ./dist and ./core/target/release.
Exit status: 0 = every check passed, 1 = a check failed, 2 = something needed is missing (nothing was checked).
Uses only the Python standard library. Everything it creates is under a temporary folder that is removed at the end.
"""
import http.server
import json
import os
import pathlib
import queue
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time

TOOL_DIRS = ["/opt/homebrew/bin", "/usr/local/bin"]
results = []


def find_tool(name):
    for d in TOOL_DIRS:
        p = os.path.join(d, name)
        if os.path.isfile(p) and os.access(p, os.X_OK):
            return p
    return None


def find_engine(arg):
    here = pathlib.Path(__file__).resolve().parents[2]
    candidates = [arg] if arg else [
        os.path.expanduser("~/Applications/Flect-for-Mac.app/Contents/Resources/flect-engine"),
        "/Applications/Flect-for-Mac.app/Contents/Resources/flect-engine",
        str(here / "dist/Flect-for-Mac.app/Contents/Resources/flect-engine"),
        str(here / "core/target/release/flect-engine"),
        str(here / "core/target/debug/flect-engine"),
    ]
    for c in candidates:
        if c and os.path.isfile(c) and os.access(c, os.X_OK):
            return os.path.abspath(c)  # the script changes folder later
    return None


def check(name, ok, detail=""):
    results.append((name, bool(ok), detail))
    print(("  PASS  " if ok else "  FAIL  ") + name + (f"   [{detail}]" if detail and not ok else ""), flush=True)


class Handler(http.server.SimpleHTTPRequestHandler):
    """Serves the folder; anything under /slow is sent in small pieces so a download can be cancelled midway."""

    def copyfile(self, source, outputfile):
        if self.path.startswith("/slow"):
            while True:
                chunk = source.read(2048)
                if not chunk:
                    break
                outputfile.write(chunk)
                outputfile.flush()
                time.sleep(0.25)
        else:
            super().copyfile(source, outputfile)

    def log_message(self, *args):
        pass

    def handle(self):
        try:
            super().handle()
        except (BrokenPipeError, ConnectionResetError):
            pass  # the client went away, which is exactly what a cancelled download does


def call(engine, env, request, cancel_when=None, timeout=90):
    """Send one request, return (events, seconds). `cancel_when(event)` -> True sends {"op":"cancel"} once."""
    proc = subprocess.Popen([engine], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                            env=env, text=True)
    lines = queue.Queue()

    def reader():
        for line in proc.stdout:
            lines.put(line)
        lines.put(None)

    threading.Thread(target=reader, daemon=True).start()
    proc.stdin.write(json.dumps(request) + "\n")
    proc.stdin.flush()
    started = time.time()
    events, cancelled = [], False
    try:
        while True:
            try:
                line = lines.get(timeout=0.2)
            except queue.Empty:
                if time.time() - started > timeout:
                    raise TimeoutError(f"no 'done' within {timeout} s")
                continue
            if line is None:
                break
            event = json.loads(line)
            events.append(event)
            if cancel_when and not cancelled and cancel_when(event):
                proc.stdin.write('{"op":"cancel"}\n')
                proc.stdin.flush()
                cancelled = True
            if event.get("event") == "done":
                break
    finally:
        try:
            proc.stdin.close()
        except OSError:
            pass
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
    return events, time.time() - started


def outputs_of(events):
    paths = []
    for e in events:
        if e.get("event") == "data" and e.get("name") == "outputs":
            paths += e["data"]["paths"]
    return paths


def keys_of(events, kind):
    return [e.get("key") for e in events if e.get("event") == kind]


def outcome_of(events):
    done = [e for e in events if e.get("event") == "done"]
    return done[-1]["outcome"] if done else None


def write_settings(flect_home, enabled, out_dir):
    flect_home.mkdir(parents=True, exist_ok=True)
    (flect_home / "settings.json").write_text(
        json.dumps({"Version": 1, "Download": {"Enabled": enabled}, "Output": {"Download": str(out_dir)}}))


def main():
    engine = find_engine(sys.argv[1] if len(sys.argv) > 1 else None)
    tools = {n: find_tool(n) for n in ("yt-dlp", "ffmpeg", "ffprobe")}
    missing = [n for n, p in tools.items() if not p]
    if not engine or missing:
        print("Cannot run the download check:")
        if not engine:
            print("  - flect-engine was not found (pass its path, or build/install the app first)")
        for n in missing:
            print(f"  - {n} was not found in {' or '.join(TOOL_DIRS)}  (install: brew install yt-dlp ffmpeg)")
        return 2
    print(f"engine : {engine}")
    for n, p in tools.items():
        flag = "--version" if n == "yt-dlp" else "-version"
        version = subprocess.run([p, flag], capture_output=True, text=True).stdout.splitlines()[:1]
        print(f"{n:7}: {p}  {version[0] if version else ''}")

    work = pathlib.Path(tempfile.mkdtemp(prefix="flect-verify-"))
    server = None
    try:
        www, out, flect_home, home, tmp = (work / d for d in ("www", "out", "flect", "home", "tmp"))
        for d in (www, home, tmp):
            d.mkdir()
        made = subprocess.run(
            [tools["ffmpeg"], "-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc=duration=3:size=640x360:rate=24",
             "-f", "lavfi", "-i", "sine=frequency=440:duration=3", "-c:v", "libx264", "-c:a", "aac",
             "-pix_fmt", "yuv420p", "-shortest", str(www / "clip.mp4")], capture_output=True, text=True)
        if made.returncode != 0:
            print("ffmpeg could not make the test clip:", made.stderr.strip())
            return 2
        shutil.copy(www / "clip.mp4", www / "slow.mp4")
        os.chdir(www)
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        base = f"http://127.0.0.1:{server.server_address[1]}"
        env = {"PATH": "/usr/bin:/bin", "HOME": str(home), "FLECT_HOME": str(flect_home), "TMPDIR": str(tmp)}

        def listing():
            return sorted(p.name for p in out.iterdir()) if out.exists() else []

        print("\n1. switched off in the settings")
        write_settings(flect_home, False, out)
        ev, _ = call(engine, env, {"op": "download", "url": f"{base}/clip.mp4", "kind": "video"})
        check("refused with download.disabled", "download.disabled" in keys_of(ev, "error"), ev)
        check("run ends as failed", outcome_of(ev) == "failed")
        check("nothing reported as created", not outputs_of(ev))
        check("nothing written", listing() == [])

        print("\n2. refused before anything runs")
        write_settings(flect_home, True, out)
        ev, _ = call(engine, env, {"op": "download", "url": "file:///etc/passwd", "kind": "audio"})
        check("a file:// address is refused (protocol.badUrl)", "protocol.badUrl" in keys_of(ev, "error"), ev)
        ev, _ = call(engine, env, {"op": "download", "url": f"{base}/clip.mp4", "kind": "video", "max_height": 999})
        check("a quality that is not offered is refused (protocol.badDownload)", "protocol.badDownload" in keys_of(ev, "error"), ev)

        print("\n3. best-quality video")
        ev, secs = call(engine, env, {"op": "download", "url": f"{base}/clip.mp4", "kind": "video"})
        files = outputs_of(ev)
        check("succeeds", outcome_of(ev) == "succeeded", ev)
        check("exactly one file reported, inside the output folder",
              len(files) == 1 and os.path.dirname(files[0]) == str(out), files)
        check("the file exists and is not empty", bool(files) and os.path.getsize(files[0]) > 0)
        check("it is an mp4", bool(files) and files[0].endswith(".mp4"), files)
        check("the status line named the host", any(e.get("event") == "status" and e.get("key") == "download.starting"
                                                   for e in ev))
        first = files[0] if files else None
        first_stat = os.stat(first) if first and os.path.exists(first) else None
        print(f"         ({secs:.1f} s, {sum(1 for e in ev if e.get('event') == 'file_step')} progress events)")

        print("\n4. the same address again, capped to 480p (the file is already there)")
        ev, _ = call(engine, env, {"op": "download", "url": f"{base}/clip.mp4", "kind": "video", "max_height": 480})
        check("succeeds", outcome_of(ev) == "succeeded", ev)
        check("the existing file is NOT reported as created (so Undo can never touch it)", not outputs_of(ev), outputs_of(ev))
        check("a download.alreadyExists warning is shown", "download.alreadyExists" in keys_of(ev, "warn"), ev)
        check("the existing file is untouched",
              first_stat is not None and os.path.exists(first) and os.stat(first).st_mtime_ns == first_stat.st_mtime_ns
              and os.stat(first).st_size == first_stat.st_size)

        print("\n5. audio only (MP3) must not touch the video of the same address")
        ev, _ = call(engine, env, {"op": "download", "url": f"{base}/clip.mp4", "kind": "audio"})
        files = outputs_of(ev)
        check("succeeds", outcome_of(ev) == "succeeded", ev)
        check("one mp3 reported, with its own name", len(files) == 1 and files[0].endswith("(audio).mp3"), files)
        check("the earlier video is still there", bool(first) and os.path.exists(first), listing())
        check("the folder holds exactly the two finished files", len(listing()) == 2, listing())
        check("no unfinished parts left",
              not any(n.endswith((".part", ".ytdl", ".temp")) or re.search(r"\.f[0-9]+\.", n) for n in listing()), listing())
        check("the private work folder is gone", not [p for p in tmp.iterdir()], [p.name for p in tmp.iterdir()])

        print("\n6. cancel in the middle of a slow download")
        before = listing()
        ev, secs = call(engine, env, {"op": "download", "url": f"{base}/slow.mp4", "kind": "video"},
                        cancel_when=lambda e: e.get("event") == "file_step")
        check("ends as cancelled", outcome_of(ev) == "cancelled", ev[-3:])
        check("stops within 10 s of the request", secs < 10, f"{secs:.1f} s")
        time.sleep(0.5)
        left = subprocess.run(["pgrep", "-f", f"{base}/slow.mp4"], capture_output=True, text=True).stdout.split()
        check("no yt-dlp / ffmpeg process is left running", not left, left)
        check("nothing was added to the output folder", listing() == before, listing())
        check("the private work folder is gone", not [p for p in tmp.iterdir()], [p.name for p in tmp.iterdir()])

        print("\n7. an address that is a playlist saves only its first item")
        for n in ("a", "b", "c"):
            shutil.copy(www / "clip.mp4", www / f"{n}.mp4")
        tracks = "".join(f"<track><location>{base}/{n}.mp4</location><title>{n}</title></track>" for n in "abc")
        (www / "list.xspf").write_text('<?xml version="1.0" encoding="UTF-8"?>'
                                       '<playlist version="1" xmlns="http://xspf.org/ns/0/"><trackList>'
                                       + tracks + "</trackList></playlist>")
        list_out = work / "out_playlist"
        write_settings(flect_home, True, list_out)
        ev, _ = call(engine, env, {"op": "download", "url": f"{base}/list.xspf", "kind": "video"})
        saved = sorted(p.name for p in list_out.iterdir()) if list_out.exists() else []
        check("succeeds", outcome_of(ev) == "succeeded", ev)
        check("one file is saved, not the whole list", len(saved) == 1 and len(outputs_of(ev)) == 1, saved)

        print("\n8. the quality limit on a stream that reports its heights (240p, 480p and 1080p versions)")
        renditions = ((240, "426x240", 300000), (480, "854x480", 900000), (1080, "1920x1080", 3000000))
        made_all = True
        for h, size, _ in renditions:
            r = subprocess.run(
                [tools["ffmpeg"], "-loglevel", "error", "-y", "-f", "lavfi", "-i", f"testsrc=duration=3:size={size}:rate=24",
                 "-f", "lavfi", "-i", "sine=frequency=440:duration=3", "-c:v", "libx264", "-c:a", "aac",
                 "-pix_fmt", "yuv420p", "-shortest", "-f", "hls", "-hls_time", "2", "-hls_playlist_type", "vod",
                 "-hls_segment_filename", f"v{h}_%d.ts", f"v{h}.m3u8"], cwd=www, capture_output=True, text=True)
            made_all = made_all and r.returncode == 0
        (www / "master.m3u8").write_text("#EXTM3U\n" + "".join(
            f"#EXT-X-STREAM-INF:BANDWIDTH={b},RESOLUTION={size}\nv{h}.m3u8\n" for h, size, b in renditions))
        check("the test stream could be made", made_all)

        def height_of(path):
            out = subprocess.run([tools["ffprobe"], "-v", "error", "-select_streams", "v:0", "-show_entries",
                                  "stream=height", "-of", "csv=p=0", path], capture_output=True, text=True).stdout.strip()
            return int(out) if out.isdigit() else None

        for label, cap, expected in (("no limit", None, 1080), ("up to 720p", 720, 480), ("up to 480p", 480, 480)):
            where = work / ("out_hls_" + label.replace(" ", "_"))
            write_settings(flect_home, True, where)
            request = {"op": "download", "url": f"{base}/master.m3u8", "kind": "video"}
            if cap:
                request["max_height"] = cap
            ev, _ = call(engine, env, request)
            files = outputs_of(ev)
            got = height_of(files[0]) if files and os.path.exists(files[0]) else None
            check(f"{label}: the {expected}p version is saved", outcome_of(ev) == "succeeded" and got == expected,
                  f"saved height {got}")
    except Exception as exc:  # a crash of the check itself is a failure, with its reason
        check("the check itself ran to the end", False, repr(exc))
    finally:
        if server:
            server.shutdown()
        os.chdir("/")
        shutil.rmtree(work, ignore_errors=True)

    failed = [r for r in results if not r[1]]
    print(f"\n{len(results) - len(failed)} of {len(results)} checks passed")
    for name, _, detail in failed:
        print(f"  FAILED: {name}   {detail}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
