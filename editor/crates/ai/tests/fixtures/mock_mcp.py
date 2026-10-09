#!/usr/bin/env python3
"""A stand-in Model Context Protocol server, for the client's tests.

Talks on its input and output, one JSON message a line. MOCK_MCP picks a
way to misbehave: `crash` dies before answering, `silent` never answers.
MOCK_MCP_LOG names a file that gets a line for every tool call. It pings
the client once and remembers whether the client answered.
"""

import json
import os
import sys
import time

mode = os.environ.get("MOCK_MCP", "")
log = os.environ.get("MOCK_MCP_LOG")
pinged = False

TOOLS = [
    {"name": "echo", "description": " Says it back. ", "inputSchema": {
        "type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}},
    {"name": "fail", "description": "Always fails.", "inputSchema": {"type": "object", "properties": {}}},
    {"name": "slow", "description": "Takes its time.", "inputSchema": {"type": "object", "properties": {}}},
    {"name": "data", "description": "Answers with data."},
]


def send(message):
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()


def reply(request, result=None, error=None):
    message = {"jsonrpc": "2.0", "id": request["id"]}
    if error:
        message["error"] = {"code": -32602, "message": error}
    else:
        message["result"] = result if result is not None else {}
    send(message)


if mode == "crash":
    sys.stderr.write("starting\nDATABASE_URL is not set\n")
    sys.exit(1)

print("a line that is not the protocol", flush=True)

for line in sys.stdin:
    try:
        message = json.loads(line)
    except ValueError:
        continue
    method = message.get("method")
    if method is None:
        # The client's answer to the ping.
        if message.get("id") == "ping-1" and "result" in message:
            pinged = True
        continue
    if mode == "silent":
        continue
    if method == "initialize":
        reply(message, {
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "mock", "version": "1"},
            "instructions": " Ask before you look. ",
        })
    elif method == "notifications/initialized":
        send({"jsonrpc": "2.0", "id": "ping-1", "method": "ping"})
        send({"jsonrpc": "2.0", "id": "roots-1", "method": "roots/list"})
        send({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"})
    elif method == "tools/list":
        cursor = (message.get("params") or {}).get("cursor")
        if cursor is None:
            reply(message, {"tools": TOOLS[:2], "nextCursor": "page-2"})
        else:
            reply(message, {"tools": TOOLS[2:]})
    elif method == "tools/call":
        params = message.get("params") or {}
        name, args = params.get("name"), params.get("arguments") or {}
        if log:
            with open(log, "a") as f:
                f.write(json.dumps([name, args, os.environ.get("MOCK_MCP_KEY")]) + "\n")
        if name == "echo":
            text = args.get("text", "")
            if text == "pinged?":
                text += " yes" if pinged else " no"
            reply(message, {"content": [
                {"type": "text", "text": text},
                {"type": "image", "data": "", "mimeType": "image/png"},
            ]})
        elif name == "fail":
            reply(message, {"content": [{"type": "text", "text": "no such row"}], "isError": True})
        elif name == "slow":
            time.sleep(1)
            reply(message, {"content": [{"type": "text", "text": "late"}]})
        elif name == "data":
            reply(message, {"content": [], "structuredContent": {"rows": 2}})
        else:
            reply(message, error="Unknown tool: %s" % name)
    elif "id" in message:
        reply(message, error="Method not found")
