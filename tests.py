#!/usr/bin/env python3
import json
import os
import queue
import subprocess
import sys
import threading
import time
import urllib.parse
from http.server import ThreadingHTTPServer, SimpleHTTPRequestHandler
from pathlib import Path


def dprint(*args):
    if os.environ.get("DEBUG"):
        print("[DEBUG]", *args, file=sys.stderr)


class LspWire:
    def __init__(self, proc: subprocess.Popen):
        self.proc = proc
        self.q = queue.Queue()
        self._t = threading.Thread(target=self._reader, daemon=True)
        self._t.start()
        self._id = 0

    def _read_exact(self, n: int) -> bytes:
        buf = b""
        while len(buf) < n:
            chunk = self.proc.stdout.read(n - len(buf))
            if not chunk:
                raise EOFError("stdout closed")
            buf += chunk
        return buf

    def _reader(self):
        try:
            while True:
                # headers
                headers = {}
                while True:
                    line = self.proc.stdout.readline()
                    if not line:
                        return
                    line = line.decode("utf-8", errors="replace").strip()
                    if line == "":
                        break
                    if ":" in line:
                        k, v = line.split(":", 1)
                        headers[k.strip().lower()] = v.strip()
                clen = int(headers.get("content-length", "0"))
                if clen <= 0:
                    continue
                body = self._read_exact(clen)
                msg = json.loads(body.decode("utf-8", errors="replace"))
                self.q.put(msg)
        except Exception as e:
            self.q.put({"__reader_error__": str(e)})

    def send(self, msg: dict):
        data = json.dumps(msg, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
        wire = f"Content-Length: {len(data)}\r\n\r\n".encode("ascii") + data
        self.proc.stdin.write(wire)
        self.proc.stdin.flush()
        dprint("send", msg.get("method") or ("response id=" + str(msg.get("id"))))

    def request(self, method: str, params: dict):
        self._id += 1
        msg = {"jsonrpc": "2.0", "id": self._id, "method": method, "params": params}
        self.send(msg)
        return self._id

    def notify(self, method: str, params: dict):
        msg = {"jsonrpc": "2.0", "method": method, "params": params}
        self.send(msg)

    def wait_for(self, predicate, timeout_s: float = 8.0):
        deadline = time.time() + timeout_s
        while True:
            remaining = deadline - time.time()
            if remaining <= 0:
                raise TimeoutError("timeout waiting for message")
            msg = self.q.get(timeout=remaining)
            if "__reader_error__" in msg:
                raise RuntimeError(f"reader error: {msg['__reader_error__']}")
            if predicate(msg):
                return msg


def file_uri(path: Path) -> str:
    p = path.resolve()
    return "file://" + urllib.parse.quote(str(p))


class QuietHandler(SimpleHTTPRequestHandler):
    def log_message(self, fmt, *args):
        # silence unless DEBUG
        if os.environ.get("DEBUG"):
            super().log_message(fmt, *args)


def start_http_server(root: Path):
    # Serve schemas from tests/http_schemas
    os.chdir(str(root))
    httpd = ThreadingHTTPServer(("127.0.0.1", 0), QuietHandler)
    port = httpd.server_address[1]
    t = threading.Thread(target=httpd.serve_forever, daemon=True)
    t.start()
    return httpd, port


def load_text(path: Path, port: int | None = None) -> str:
    txt = path.read_text(encoding="utf-8")
    if port is not None:
        txt = txt.replace("__PORT__", str(port))
    return txt


def load_golden(path: Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def assert_contains_all(haystack: str, needles: list[str], ctx: str):
    for n in needles:
        if n not in haystack:
            raise AssertionError(f"{ctx}: missing substring: {n!r}\n---\n{haystack}\n---")


def range_is_default(diag: dict) -> bool:
    r = diag.get("range") or {}
    s = r.get("start") or {}
    e = r.get("end") or {}
    return (
        s.get("line", 0) == 0
        and s.get("character", 0) == 0
        and e.get("line", 0) == 0
        and e.get("character", 0) == 0
    )


def main() -> int:
    repo = Path(__file__).resolve().parent
    cases = repo / "tests" / "cases"
    http_root = repo / "tests" / "http_schemas"

    # Ensure binary exists
    exe = "jylsp.exe" if os.name == "nt" else "jylsp"
    bin_path = repo / "target" / "release" / exe
    if not bin_path.exists():
        dprint("building cargo project")
        subprocess.run(["cargo", "build"], cwd=str(repo), check=True)
    if not bin_path.exists():
        print(f"error: binary not found at {bin_path}", file=sys.stderr)
        return 2

    httpd, port = start_http_server(http_root)
    dprint("http schema server port", port)

    env = os.environ.copy()
    # Keep LSP stdout clean; Rust debug prints are on stderr and gated by DEBUG.
    # env["DEBUG"] = env.get("DEBUG", "")

    proc = subprocess.Popen(
        [str(bin_path), "--stdio"],
        cwd=str(repo),
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
    )

    # Drain stderr to avoid deadlocks and show when DEBUG
    def _stderr_pump():
        for line in iter(proc.stderr.readline, b""):
            if os.environ.get("DEBUG"):
                sys.stderr.write(line.decode("utf-8", errors="replace"))
                sys.stderr.flush()

    threading.Thread(target=_stderr_pump, daemon=True).start()

    wire = LspWire(proc)

    # initialize
    init_id = wire.request(
        "initialize",
        {
            "processId": os.getpid(),
            "rootUri": file_uri(repo),
            "capabilities": {
                "textDocument": {"synchronization": {"didSave": True}},
            },
            "trace": "off",
        },
    )
    resp = wire.wait_for(lambda m: m.get("id") == init_id, timeout_s=10.0)
    if "error" in resp:
        raise RuntimeError(f"initialize failed: {resp['error']}")

    wire.notify("initialized", {})

    # Run cases
    case_files = sorted([p for p in cases.iterdir() if p.suffix in (".json", ".yml", ".yaml")])
    failures = 0

    for p in case_files:
        uri = file_uri(p)
        text = load_text(p, port=port)

        dprint("open", p.name, uri)
        wire.notify(
            "textDocument/didOpen",
            {
                "textDocument": {
                    "uri": uri,
                    "languageId": "json" if p.suffix == ".json" else "yaml",
                    "version": 1,
                    "text": text,
                }
            },
        )

        diag_msg = wire.wait_for(
            lambda m: m.get("method") == "textDocument/publishDiagnostics"
            and (m.get("params") or {}).get("uri") == uri,
            timeout_s=10.0,
        )
        diags = (diag_msg.get("params") or {}).get("diagnostics") or []

        golden_path = p.with_suffix(p.suffix + ".golden")
        if golden_path.exists():
            golden = load_golden(golden_path)
            expect_errors = bool(golden.get("expect_errors", True))
            must_contain = golden.get("must_contain", [])
            must_not_default_range = bool(golden.get("must_not_default_range", False))

            messages = "\n".join([d.get("message", "") for d in diags])
            if expect_errors and not diags:
                print(f"FAIL {p.name}: expected diagnostics but got none", file=sys.stderr)
                failures += 1
            if must_contain:
                try:
                    assert_contains_all(messages, must_contain, f"{p.name}")
                except AssertionError as e:
                    print(f"FAIL {p.name}: {e}", file=sys.stderr)
                    failures += 1
            if must_not_default_range and diags:
                if range_is_default(diags[0]):
                    print(f"FAIL {p.name}: expected non-default range", file=sys.stderr)
                    failures += 1
        else:
            if diags:
                print(f"FAIL {p.name}: expected no diagnostics, got {len(diags)}", file=sys.stderr)
                for d in diags:
                    print("  -", d.get("message", ""), file=sys.stderr)
                failures += 1

        wire.notify("textDocument/didClose", {"textDocument": {"uri": uri}})

    # shutdown
    shut_id = wire.request("shutdown", {})
    wire.wait_for(lambda m: m.get("id") == shut_id, timeout_s=5.0)
    wire.notify("exit", {})

    try:
        proc.wait(timeout=5.0)
    except subprocess.TimeoutExpired:
        proc.terminate()

    httpd.shutdown()

    if failures:
        print(f"{failures} test(s) failed", file=sys.stderr)
        return 1

    print("OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
