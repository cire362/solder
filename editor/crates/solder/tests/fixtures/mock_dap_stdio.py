#!/usr/bin/env python3
"""A stand-in debug adapter that talks on its input and output, as the
adapters VS Code extensions declare do.

It is one session: it stops on the first breakpoint set in the program it
was launched for and ends on `continue`. Its first argument names a file
that gets the launch it was given.
"""

import json
import sys

log = sys.argv[1] if len(sys.argv) > 1 else None
out = sys.stdout.buffer
seq = 0
launched = {}
lines = {}


def send(message):
    global seq
    seq += 1
    message["seq"] = seq
    body = json.dumps(message).encode()
    out.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
    out.flush()


def event(name, body=None):
    send({"type": "event", "event": name, "body": body or {}})


def read():
    length = 0
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            break
        name, _, value = line.partition(b":")
        if name.lower() == b"content-length":
            length = int(value)
    return json.loads(sys.stdin.buffer.read(length))


while True:
    request = read()
    if request is None:
        break
    command, args = request.get("command"), request.get("arguments") or {}
    body = {}
    if command == "initialize":
        body = {"supportsConfigurationDoneRequest": True}
    elif command == "launch":
        launched = args
        if log:
            with open(log, "w") as f:
                json.dump(args, f)
    elif command == "setBreakpoints":
        path = args.get("source", {}).get("path")
        lines[path] = [b["line"] for b in args.get("breakpoints", [])]
        body = {"breakpoints": [{"verified": True, "line": line} for line in lines[path]]}
    elif command == "threads":
        body = {"threads": [{"id": 1, "name": "main"}]}
    elif command == "stackTrace":
        program = launched.get("program")
        body = {"stackFrames": [{
            "id": 1, "name": "main", "line": (lines.get(program) or [1])[0], "column": 1,
            "source": {"path": program, "name": "program"},
        }], "totalFrames": 1}
    elif command == "scopes":
        body = {"scopes": []}
    send({"type": "response", "request_seq": request["seq"], "success": True,
          "command": command, "body": body})
    if command == "initialize":
        event("initialized")
    elif command == "configurationDone":
        if lines.get(launched.get("program")):
            event("stopped", {"reason": "breakpoint", "threadId": 1, "allThreadsStopped": True})
        else:
            event("terminated")
    elif command == "continue":
        event("terminated")
    elif command in ("disconnect", "terminate"):
        break
