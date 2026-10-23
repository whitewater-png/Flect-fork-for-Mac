#!/usr/bin/env python3
"""Serves a folder on 127.0.0.1 only, for trying the web download without any third-party site.

    python3 scripts/mac-verify/slow_server.py            slow (about 40 KB/s), so a download can be cancelled midway
    python3 scripts/mac-verify/slow_server.py --fast     normal speed
    python3 scripts/mac-verify/slow_server.py --dir ~/FlectTest --port 8000

Then, in Flect, download http://127.0.0.1:8000/sample.mp4 (make ~/FlectTest first with scripts/mac-verify.sh).
Stop it with Ctrl-C. Nothing is reachable from other machines (it binds to 127.0.0.1).
"""
import argparse
import functools
import http.server
import os
import time


class Handler(http.server.SimpleHTTPRequestHandler):
    slow = True

    def copyfile(self, source, outputfile):
        if not self.slow:
            return super().copyfile(source, outputfile)
        while True:
            chunk = source.read(2048)
            if not chunk:
                break
            outputfile.write(chunk)
            outputfile.flush()
            time.sleep(0.05)

    def handle(self):
        try:
            super().handle()
        except (BrokenPipeError, ConnectionResetError):
            pass  # the client went away, which is what a cancelled download does


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--dir", default="~/FlectTest")
    parser.add_argument("--port", type=int, default=8000)
    parser.add_argument("--fast", action="store_true")
    args = parser.parse_args()
    folder = os.path.expanduser(args.dir)
    if not os.path.isdir(folder):
        raise SystemExit(f"{folder} does not exist; run scripts/mac-verify.sh first")
    Handler.slow = not args.fast
    handler = functools.partial(Handler, directory=folder)
    with http.server.ThreadingHTTPServer(("127.0.0.1", args.port), handler) as server:
        speed = "normal speed" if args.fast else "slow (about 40 KB/s)"
        print(f"Serving {folder} at http://127.0.0.1:{args.port}/ ({speed}). Ctrl-C to stop.")
        try:
            server.serve_forever()
        except KeyboardInterrupt:
            print()


if __name__ == "__main__":
    main()
