#!/usr/bin/env python3
"""A stand-in for js-debug's DAP server, for the debugger's UI tests.

Run as `mock_dap.py <port> 127.0.0.1`. Like js-debug, each launch is a
parent session that asks the client with `startDebugging` to open a child
session on a new connection; the child runs the program. A Node child stops
on its first breakpoint, steps one line at a time, and ends on `continue`.
A launch's env may name a SOLDER_TIMELINE file; one query is recorded there.
"""

import json
import socket
import sys
import threading

port = int(sys.argv[1])
targets = {}
lock = threading.Lock()


class Session:
    def __init__(self, conn):
        self.conn = conn
        self.file = conn.makefile("rb")
        self.seq = 1
        self.config = {}
        self.child = False
        self.program = None
        self.lines = []
        self.line = 0
        self.launch = None
        self.parent = None
        self.sending = threading.Lock()

    def send(self, message):
        # A child's end is also sent on its parent's connection.
        with self.sending:
            message["seq"] = self.seq
            self.seq += 1
            body = json.dumps(message).encode()
            self.conn.sendall(b"Content-Length: %d\r\n\r\n" % len(body) + body)

    def event(self, name, body=None):
        self.send({"type": "event", "event": name, "body": body or {}})

    def reply(self, request, body=None, success=True):
        self.send({
            "type": "response",
            "request_seq": request["seq"],
            "command": request["command"],
            "success": success,
            "body": body or {},
        })

    def read(self):
        length = None
        while True:
            line = self.file.readline()
            if not line:
                return None
            line = line.strip()
            if not line:
                break
            if line.lower().startswith(b"content-length:"):
                length = int(line.split(b":")[1])
        return json.loads(self.file.read(length))

    def stop_at(self, line, reason):
        self.line = line
        self.event("stopped", {"reason": reason, "threadId": 1})

    def finish(self):
        # The parent's end first, the worst order for a client: the child's
        # last output must still arrive.
        if self.parent:
            self.parent.event("terminated")
        self.event("output", {"category": "stdout", "output": "done\n"})
        self.event("exited", {"exitCode": 0})
        self.event("terminated")

    def handle(self, request):
        command = request["command"]
        args = request.get("arguments") or {}
        if command == "initialize":
            self.reply(request, {"supportsConfigurationDoneRequest": True})
        elif command == "launch":
            self.config = args
            pending = args.get("__pendingTargetId")
            if pending:
                self.child = True
                with lock:
                    self.program, self.parent = targets.get(pending, (None, None))
                self.reply(request)
            else:
                self.launch = request
            # A parent answers launch after configurationDone, as js-debug.
            self.event("initialized")
        elif command == "setBreakpoints":
            path = args["source"].get("path")
            lines = [b["line"] for b in args.get("breakpoints", [])]
            if path == self.program:
                self.lines = sorted(lines)
            ids = [hash((path, line)) % 100000 for line in lines]
            self.reply(request, {"breakpoints": [
                {"id": i, "verified": False, "line": line} for i, line in zip(ids, lines)
            ]})
            # Verified once the script loads.
            for i, line in zip(ids, lines):
                self.event("breakpoint", {"reason": "changed", "breakpoint": {
                    "id": i, "verified": True, "line": line,
                }})
        elif command == "configurationDone":
            self.reply(request)
            if not self.child:
                self.reply(self.launch)
                self.start_child()
            elif self.config.get("type") == "pwa-node":
                self.event("output", {"category": "stdout", "output": "ready on http://localhost:4123\n"})
                if self.lines:
                    self.stop_at(self.lines[0], "breakpoint")
                else:
                    self.finish()
        elif command == "threads":
            self.reply(request, {"threads": [{"id": 1, "name": "main"}]})
        elif command == "stackTrace":
            self.reply(request, {"stackFrames": [
                {"id": 1, "name": "add", "line": self.line, "column": 1,
                 "source": {"path": self.program}},
                {"id": 2, "name": "processTicks", "line": 1, "column": 1,
                 "source": {"name": "<node_internals>", "presentationHint": "deemphasize"}},
            ]})
        elif command == "scopes":
            self.reply(request, {"scopes": [{"name": "Local", "variablesReference": 10}]})
        elif command == "variables":
            ref = args.get("variablesReference")
            if ref == 10:
                variables = [
                    {"name": "a", "value": "2", "variablesReference": 0},
                    {"name": "user", "value": "{name: 'Ada'}", "variablesReference": 11},
                ]
            else:
                variables = [{"name": "name", "value": "'Ada'", "variablesReference": 0}]
            self.reply(request, {"variables": variables})
        elif command == "evaluate":
            self.reply(request, {"result": "seen " + args.get("expression", ""), "variablesReference": 0})
        elif command in ("next", "stepIn", "stepOut"):
            self.reply(request)
            self.event("continued", {"threadId": 1})
            self.stop_at(self.line + 1, "step")
        elif command == "pause":
            self.reply(request)
            self.stop_at(self.line or 1, "pause")
        elif command == "continue":
            self.reply(request, {"allThreadsContinued": True})
            self.event("continued", {"threadId": 1})
            self.finish()
        elif command == "disconnect":
            self.reply(request)
            self.event("terminated")
            return False
        else:
            self.reply(request, success=False)
        return True

    def start_child(self):
        target = "t%d" % id(self)
        with lock:
            # What is debugged, under the name js-debug has for it or the
            # one Ruby's adapter does.
            program = self.config.get("program") or self.config.get("script_or_command")
            targets[target] = (program, self)
        timeline = (self.config.get("env") or {}).get("SOLDER_TIMELINE")
        if timeline:
            with open(timeline, "a") as f:
                f.write(json.dumps({
                    "kind": "sql", "label": "select 1", "at": 1, "ms": 2,
                    "path": self.config.get("program"), "line": 2,
                }) + "\n")
        name = "about:blank" if self.config.get("type") == "pwa-chrome" else "app.js [1]"
        self.send({
            "type": "request",
            "command": "startDebugging",
            "arguments": {
                "request": "launch",
                "configuration": {
                    # A program that is not a browser's page runs as Node's
                    # does, whatever the adapter calls itself.
                    "type": self.config.get("type") or "pwa-node",
                    "name": name,
                    "__pendingTargetId": target,
                },
            },
        })

    def run(self):
        try:
            while True:
                message = self.read()
                if message is None:
                    break
                if message.get("type") == "request" and not self.handle(message):
                    break
        except (OSError, ValueError):
            pass
        finally:
            self.conn.close()


server = socket.socket()
server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
server.bind(("127.0.0.1", port))
server.listen()
while True:
    conn, _ = server.accept()
    threading.Thread(target=Session(conn).run, daemon=True).start()
