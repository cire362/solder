#!/usr/bin/env python3
"""Stands in for llama-server in tests: same flags, /health and /completion
with timings. A model named missing.gguf fails to load, as the real server
does."""
import json
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
        elif self.path == "/v1/chat/completions":
            # Streams back the last question, word by word.
            question = body["messages"][-1]["content"].splitlines()[-1]
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            for word in ("Local answer to: " + question).split(" "):
                chunk = {"choices": [{"delta": {"content": word + " "}}]}
                self.wfile.write(f"data: {json.dumps(chunk)}\n\n".encode())
                self.wfile.flush()
            self.wfile.write(b"data: [DONE]\n\n")
        else:
            self.reply(404, {})


print(f"main: server is listening on http://127.0.0.1:{port}", flush=True)
HTTPServer(("127.0.0.1", port), Handler).serve_forever()
