#!/usr/bin/env python3
"""A stand-in Model Context Protocol server reached over HTTP, for the
client's tests.

Listens on a port of its own choosing on this machine and prints it as its
first line. Every message is a POST to `/mcp`. It wants the key `Bearer t`
and, after the greeting, the name it gave the conversation. Lists come as
streams of events, with a ping of its own before the answer; the rest as
plain answers. MOCK_MCP_LOG names a file that gets a line for what it is
told besides requests.
"""

import json
import os
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

log = os.environ.get("MOCK_MCP_LOG")
SESSION = "talk-1"
state = {"pinged": False}

TOOLS = [
    {"name": "echo", "description": "Says it back.", "inputSchema": {
        "type": "object", "properties": {"text": {"type": "string"}}}},
    {"name": "slow", "description": "Takes its time.", "inputSchema": {"type": "object"}},
]
PROMPTS = [
    {"name": "review", "description": " Reviews a change. ", "arguments": [
        {"name": "branch", "description": "The branch to review", "required": True},
        {"name": "tone"},
    ]},
    {"name": "standup"},
]
RESOURCES = [
    {"uri": "notes://today", "name": "Today", "description": "What is planned"},
    {"uri": "notes://logo", "mimeType": "image/png"},
]


def note(what):
    if log:
        with open(log, "a") as f:
            f.write(json.dumps(what) + "\n")


def answer(message):
    method, params = message.get("method"), message.get("params") or {}
    if method == "initialize":
        return {
            "protocolVersion": "2025-06-18",
            "capabilities": {"tools": {}, "prompts": {}, "resources": {}},
            "serverInfo": {"name": "mock-http", "version": "1"},
        }
    if method == "tools/list":
        return {"tools": TOOLS}
    if method == "tools/call":
        name, args = params.get("name"), params.get("arguments") or {}
        if name == "echo":
            text = args.get("text", "")
            if text == "pinged?":
                text += " yes" if state["pinged"] else " no"
            return {"content": [{"type": "text", "text": text}]}
        if name == "slow":
            time.sleep(1)
            return {"content": [{"type": "text", "text": "late"}]}
        return None
    if method == "prompts/list":
        return {"prompts": PROMPTS}
    if method == "prompts/get":
        args = params.get("arguments") or {}
        if params.get("name") == "review":
            return {"messages": [
                {"role": "user", "content": {"type": "text", "text": "Review %s." % args.get("branch")}},
                {"role": "user", "content": [{"type": "text", "text": "Be %s." % args.get("tone", "kind")}]},
            ]}
        return None
    if method == "resources/list":
        return {"resources": RESOURCES}
    if method == "resources/read":
        uri = params.get("uri")
        if uri == "notes://today":
            return {"contents": [{"uri": uri, "text": "Ship the layout."}]}
        if uri == "notes://logo":
            return {"contents": [{"uri": uri, "mimeType": "image/png", "blob": "AAAA"}]}
        return None
    return None


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_):
        pass

    def plain(self, status, body=b"", kind="application/json", extra=None):
        self.send_response(status)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(body)))
        for name, value in (extra or {}).items():
            self.send_header(name, value)
        self.end_headers()
        self.wfile.write(body)

    def do_DELETE(self):
        note(["ended", self.headers.get("Mcp-Session-Id")])
        self.plain(200)

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        message = json.loads(self.rfile.read(length) or b"{}")
        if self.path != "/mcp":
            return self.plain(404)
        if self.headers.get("Authorization") != "Bearer t":
            return self.plain(401)
        method = message.get("method")
        if method != "initialize" and self.headers.get("Mcp-Session-Id") != SESSION:
            return self.plain(400)
        if method != "initialize" and self.headers.get("MCP-Protocol-Version") != "2025-06-18":
            return self.plain(400)
        if method is None:
            # The client's answer to the ping.
            if message.get("id") == "ping-1" and "result" in message:
                state["pinged"] = True
            return self.plain(202)
        if "id" not in message:
            note(["told", method])
            return self.plain(202)
        result = answer(message)
        reply = {"jsonrpc": "2.0", "id": message["id"]}
        if result is None:
            reply["error"] = {"code": -32602, "message": "Unknown: %s" % method}
        else:
            reply["result"] = result
        extra = {"Mcp-Session-Id": SESSION} if method == "initialize" else {}
        if not method.endswith("/list"):
            return self.plain(200, json.dumps(reply).encode(), extra=extra)
        # A stream: a comment, a question of its own, a notification, and
        # the answer in two lines of one event, each piece sent apart.
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Connection", "close")
        self.end_headers()
        text = json.dumps(reply)
        half = text.index(",") + 1
        pieces = [
            ": hello\n\n",
            "event: message\ndata: %s\n\n" % json.dumps({"jsonrpc": "2.0", "id": "ping-1", "method": "ping"}),
            "data: %s\r\n\r\n" % json.dumps({"jsonrpc": "2.0", "method": "notifications/progress"}),
            "id: 3\ndata: %s\n" % text[:half],
            "data: %s\n\n" % text[half:],
        ]
        for piece in pieces:
            self.wfile.write(piece.encode())
            self.wfile.flush()
            time.sleep(0.02)
        self.close_connection = True


server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
print(server.server_address[1], flush=True)
server.serve_forever()
