#!/usr/bin/env python3
"""Record the wire messages while running Solder's existing mock server."""
import json
import os
import runpy
import sys

fixture = sys.argv.pop(1)
root_uri = os.environ["MAPPED_LSP_ROOT_URI"]
root_path = json.loads(os.environ["MAPPED_LSP_ROOT_PATH"])
document_uri = os.environ["MAPPED_LSP_DOCUMENT_URI"]
configuration_id = root_uri + "/configuration"


def send(message):
    body = json.dumps(message).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
    sys.stdout.buffer.flush()


class Wire:
    def __init__(self, buffer):
        self.buffer = buffer

    def readline(self):
        return self.buffer.readline()

    def read(self, size):
        body = self.buffer.read(size)
        message = json.loads(body)
        with open(os.environ["MAPPED_LSP_WIRE_LOG"], "a", encoding="utf-8") as log:
            log.write(json.dumps(message) + "\n")
        method = message.get("method")
        params = message.get("params") or {}
        if method == "initialize":
            assert params["rootUri"] == root_uri
            assert params.get("rootPath") == root_path
            assert params["workspaceFolders"][0]["uri"] == root_uri
        if "textDocument" in params:
            assert params["textDocument"]["uri"] == document_uri
        if method == "initialized":
            send({"jsonrpc": "2.0", "id": configuration_id,
                  "method": "workspace/configuration", "params": {
                      "items": [{"section": "demo", "scopeUri": document_uri}]}})
            # Outside-root navigation is deliberately left for the parent.
            send({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics",
                  "params": {"uri": "file://other-machine/outside.rs", "diagnostics": []}})
        if method is None and message.get("id") == configuration_id:
            assert message["result"][0]["uri"] == document_uri
        if method is None and message.get("id") == 9001:
            assert message["result"]["uri"] == document_uri
        return body


class Input:
    def __init__(self, buffer):
        self.buffer = Wire(buffer)


sys.stdin = Input(sys.stdin.buffer)
runpy.run_path(fixture, run_name="__main__")
