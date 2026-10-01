#!/usr/bin/env python3
"""Stands in for llama-server in tests: same flags, /health and /completion
with timings. A model named missing.gguf fails to load, as the real server
does."""
import json
import os
import threading
import sys
import time
from http.server import BaseHTTPRequestHandler, HTTPServer

args = sys.argv[1:]


def flag(name):
    return args[args.index(name) + 1] if name in args else None


model = flag("--model")
port = int(flag("--port") or 8080)
key = flag("--api-key")
started = time.time()

if model and model.endswith("missing.gguf"):
    print("llama_model_load: error loading model", flush=True)
    print(f"main: failed to load model '{model}'", flush=True)
    sys.exit(1)


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def reply(self, status, body):
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        if self.path == "/health":
            # Loading for a moment, like the real server.
            if time.time() - started < 0.3:
                self.reply(503, {"error": {"message": "Loading model"}})
            else:
                self.reply(200, {"status": "ok"})
        else:
            self.reply(404, {})

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        body = json.loads(self.rfile.read(length) or b"{}")
        if key and self.headers.get("Authorization") != f"Bearer {key}":
            self.reply(401, {"error": {"message": "Invalid API Key"}})
            return
        print("authorized", self.path, flush=True)
        if self.path == "/completion":
            self.reply(200, {
                "content": "x" * body.get("n_predict", 1),
                "timings": {"prompt_per_second": 1234.5, "predicted_per_second": 67.8},
            })
        elif self.path == "/infill":
            # Models without fill-in-the-middle tokens refuse, like llama.cpp.
            if "nofim" in (model or ""):
                self.reply(501, {"error": {"message": "infill not supported"}})
                return
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            pieces = ["return", " a + b;"]
            for i, piece in enumerate(pieces):
                chunk = {"content": piece, "stop": i + 1 == len(pieces)}
                self.wfile.write(f"data: {json.dumps(chunk)}\n\n".encode())
                self.wfile.flush()
        elif self.path == "/v1/chat/completions":
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            system = body["messages"][0]["content"] if body["messages"] else ""
            if "<CURSOR>" in system:
                # A completion asked through chat: fenced, as chat models do.
                words = ["```js\n", "a * b;", "\n```"]
            else:
                # Streams back the last question, word by word.
                question = body["messages"][-1]["content"].splitlines()[-1]
                words = [w + " " for w in ("Local answer to: " + question).split(" ")]
            for word in words:
                chunk = {"choices": [{"delta": {"content": word}}]}
                self.wfile.write(f"data: {json.dumps(chunk)}\n\n".encode())
                self.wfile.flush()
            self.wfile.write(b"data: [DONE]\n\n")
        else:
            self.reply(404, {})


def watch_parent():
    # A test that panics never stops its server; leave with the test process.
    parent = os.getppid()
    while os.getppid() == parent:
        time.sleep(0.5)
    os._exit(0)


threading.Thread(target=watch_parent, daemon=True).start()
print(f"main: server is listening on http://127.0.0.1:{port}", flush=True)
HTTPServer(("127.0.0.1", port), Handler).serve_forever()
