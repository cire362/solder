#!/usr/bin/env python3
"""A small language server for tests. Speaks real JSON-RPC over stdio.

Diagnostics: every "TODO" is a warning, every "boom" an error.
Positions are UTF-8 byte columns (negotiated via positionEncoding).
"""
import json
import os
import re
import sys

docs = {}


def read():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line = line.decode().strip()
        if not line:
            break
        if line.lower().startswith("content-length:"):
            length = int(line.split(":")[1])
    return json.loads(sys.stdin.buffer.read(length))


def send(msg):
    body = json.dumps(msg).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
    sys.stdout.buffer.flush()


def offset(text, pos):
    lines = text.encode().split(b"\n")
    before = b"\n".join(lines[: pos["line"]])
    base = len(before) + (1 if pos["line"] > 0 else 0)
    return base + pos["character"]


def position(text, byte):
    head = text.encode()[:byte]
    line = head.count(b"\n")
    col = len(head) - (head.rfind(b"\n") + 1)
    return {"line": line, "character": col}


def rng(text, start, end):
    return {"start": position(text, start), "end": position(text, end)}


# A test that runs two servers on one file starts the second with
# MOCK_LSP_TAG set. Tagged, the server reports other words, offers another
# completion and names its actions after itself, so the test can tell whose
# answer is whose. UTF-16 positions make its ranges differ from the first's.
TAG = os.environ.get("MOCK_LSP_TAG", "")
# Set for the server that plays Vue's: like it, it has the TypeScript server
# asked something through the editor whenever a file opens or changes, and
# writes down what comes back.
ASKS_TSSERVER = bool(os.environ.get("MOCK_LSP_ASKS_TSSERVER"))
# Started with --hints, the server also draws into the text: a hint after
# the name of every function, and a color for every `helper` and `TODO`.
# And it offers two things to do with the line of every function: one it
# runs itself, said only when asked, and one it leaves to the editor.
HINTS = "--hints" in sys.argv


def ask_tsserver(uri):
    if ASKS_TSSERVER:
        send({"jsonrpc": "2.0", "method": "tsserver/request",
              "params": [[7, "_vue:projectInfo", {"file": uri}]]})


def publish(uri):
    text = docs[uri]
    diags = []
    for word, severity in ((("FIXME", 2),) if TAG else (("TODO", 2), ("boom", 1))):
        for m in re.finditer(word, text):
            diags.append({"range": rng(text, m.start(), m.end()), "severity": severity,
                          "message": f"found {word}", "source": TAG or "mock"})
    send({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics",
          "params": {"uri": uri, "diagnostics": diags}})


def note(what, value):
    """Appends what the client sent to the file MOCK_LSP_LOG names, if any."""
    log = os.environ.get("MOCK_LSP_LOG")
    if log:
        with open(log, "a") as f:
            f.write(json.dumps([what, value]) + "\n")


def word_at(text, byte):
    data = text.encode()
    start = byte
    while start > 0 and (chr(data[start - 1]).isalnum() or data[start - 1] == ord("_")):
        start -= 1
    end = byte
    while end < len(data) and (chr(data[end]).isalnum() or data[end] == ord("_")):
        end += 1
    return data[start:end].decode()


while True:
    msg = read()
    if msg is None:
        break
    method = msg.get("method")
    params = msg.get("params") or {}
    mid = msg.get("id")
    if method == "initialize":
        note("initialize", params.get("initializationOptions"))
        send({"jsonrpc": "2.0", "id": mid, "result": {"capabilities": {
            "positionEncoding": "utf-16" if TAG else "utf-8",
            "textDocumentSync": 2,
            "completionProvider": {"triggerCharacters": ["."]},
            "hoverProvider": True,
            "definitionProvider": True,
            "referencesProvider": True,
            "documentSymbolProvider": True,
            "workspaceSymbolProvider": True,
            "renameProvider": True,
            "documentFormattingProvider": True,
            "codeActionProvider": {"resolveProvider": True},
            "executeCommandProvider": {"commands": ["mock.touch"]},
            "signatureHelpProvider": {"triggerCharacters": ["("], "retriggerCharacters": [","]},
            **({
                "inlayHintProvider": True,
                "codeLensProvider": {"resolveProvider": True},
                "semanticTokensProvider": {
                    "legend": {"tokenTypes": ["function", "comment", "somethingElse"], "tokenModifiers": []},
                    "full": True,
                },
            } if HINTS else {}),
        }}})
    elif method == "workspace/didChangeConfiguration":
        note("configuration", params.get("settings"))
    elif method == "textDocument/didOpen":
        doc = params["textDocument"]
        note("open", doc.get("languageId"))
        docs[doc["uri"]] = doc["text"]
        publish(doc["uri"])
        ask_tsserver(doc["uri"])
    elif method == "textDocument/didChange":
        uri = params["textDocument"]["uri"]
        text = docs[uri]
        for change in params["contentChanges"]:
            if "range" in change:
                data = text.encode()
                start = offset(text, change["range"]["start"])
                end = offset(text, change["range"]["end"])
                text = (data[:start] + change["text"].encode() + data[end:]).decode()
            else:
                text = change["text"]
        docs[uri] = text
        publish(uri)
        ask_tsserver(uri)
    elif method == "textDocument/completion":
        send({"jsonrpc": "2.0", "id": mid, "result": [
            {"label": "println", "kind": 3, "insertText": "println!(\"$1\")", "insertTextFormat": 2},
            {"label": "print", "kind": 3, "detail": "macro"},
            {"label": "helper", "kind": 3},
        ] if not TAG else [
            {"label": f"{TAG}_println", "kind": 3},
            {"label": f"{TAG}_title", "kind": 10, "detail": "a prop"},
        ]})
    elif method == "textDocument/inlayHint":
        text = docs[params["textDocument"]["uri"]]
        hints = []
        at = text.find("fn ")
        while at >= 0:
            end = at + 3
            while end < len(text) and (text[end].isalnum() or text[end] == "_"):
                end += 1
            hints.append({
                "position": position(text, len(text[:end].encode())),
                "label": [{"value": ": "}, {"value": "fn"}],
                "paddingRight": True,
            })
            at = text.find("fn ", end)
        send({"jsonrpc": "2.0", "id": mid, "result": hints})
    elif method == "textDocument/codeLens":
        uri = params["textDocument"]["uri"]
        text = docs[uri]
        lenses = []
        at = text.find("fn ")
        while at >= 0:
            start = position(text, len(text[:at].encode()))
            lenses.append({"range": {"start": start, "end": start}, "data": uri})
            lenses.append({"range": {"start": start, "end": start},
                           "command": {"title": "Run in the editor", "command": "mock.client"}})
            at = text.find("fn ", at + 3)
        # And two the editor does itself, where a line says TODO: places
        # to show, and something to run in a terminal.
        todo = text.find("TODO")
        if todo >= 0:
            start = position(text, len(text[:todo].encode()))
            here = {"start": start, "end": start}
            first = {"line": 0, "character": 0}
            lenses.append({"range": here, "command": {
                "title": "2 places", "command": "editor.action.showReferences",
                "arguments": [uri, start, [{"uri": uri, "range": {"start": first, "end": first}},
                                           {"uri": uri, "range": here}]]}})
            lenses.append({"range": here, "command": {
                "title": "Run", "command": "rust-analyzer.runSingle",
                "arguments": [{"label": "lens run", "kind": "shell", "args": {
                    "program": "/bin/sh", "args": ["-c", "echo lens-$((40+2))"]}}]}})
        send({"jsonrpc": "2.0", "id": mid, "result": lenses})
    elif method == "codeLens/resolve":
        params["command"] = {"title": "Touch", "command": "mock.touch", "arguments": [params["data"]]}
        send({"jsonrpc": "2.0", "id": mid, "result": params})
    elif method == "textDocument/semanticTokens/full":
        text = docs[params["textDocument"]["uri"]]
        data = []
        last = (0, 0)
        for number, line in enumerate(text.split("\n")):
            found = []
            for word, kind in (("fn", 2), ("helper", 0), ("TODO", 0)):
                at = line.find(word)
                if at >= 0:
                    found.append((len(line[:at].encode()), len(word.encode()), kind))
            for start, length, kind in sorted(found):
                data += [number - last[0], start - last[1] if number == last[0] else start, length, kind, 0]
                last = (number, start)
        send({"jsonrpc": "2.0", "id": mid, "result": {"data": data}})
    elif method == "textDocument/hover":
        send({"jsonrpc": "2.0", "id": mid, "result": {"contents": {
            "kind": "markdown", "value": "```rust\nfn helper()\n```\nDoes help."}}})
    elif method == "textDocument/definition":
        uri = params["textDocument"]["uri"]
        text = docs[uri]
        start = text.find("helper")
        send({"jsonrpc": "2.0", "id": mid,
              "result": {"uri": uri, "range": rng(text, start, start + len("helper"))}})
    elif method == "textDocument/documentSymbol":
        # Every function of the file, inside one module.
        uri = params["textDocument"]["uri"]
        text = docs[uri]
        functions = [{
            "name": m.group(1), "kind": 12,
            "range": rng(text, m.start(), m.end()),
            "selectionRange": rng(text, m.start(1), m.end(1)),
        } for m in re.finditer(r"fn (\w+)", text)]
        send({"jsonrpc": "2.0", "id": mid, "result": [{
            "name": "crate", "kind": 2, "children": functions,
            "range": rng(text, 0, len(text)), "selectionRange": rng(text, 0, 0),
        }]})
    elif method == "workspace/symbol":
        # The functions of every open file whose name has the query in it.
        query = params.get("query", "")
        found = [{
            "name": m.group(1), "kind": 12, "containerName": "crate",
            "location": {"uri": uri, "range": rng(text, m.start(1), m.end(1))},
        } for uri, text in sorted(docs.items()) for m in re.finditer(r"fn (\w+)", text) if query in m.group(1)]
        send({"jsonrpc": "2.0", "id": mid, "result": found})
    elif method == "textDocument/references":
        uri = params["textDocument"]["uri"]
        text = docs[uri]
        locs = [{"uri": uri, "range": rng(text, m.start(), m.end())} for m in re.finditer("helper", text)]
        send({"jsonrpc": "2.0", "id": mid, "result": locs})
    elif method == "textDocument/rename":
        uri = params["textDocument"]["uri"]
        text = docs[uri]
        word = word_at(text, offset(text, params["position"]))
        edits = [{"range": rng(text, m.start(), m.end()), "newText": params["newName"]}
                 for m in re.finditer(re.escape(word), text)]
        send({"jsonrpc": "2.0", "id": mid, "result": {"changes": {uri: edits}}})
    elif method == "textDocument/formatting":
        uri = params["textDocument"]["uri"]
        text = docs[uri]
        formatted = "\n".join(line.rstrip() for line in text.split("\n"))
        send({"jsonrpc": "2.0", "id": mid, "result": [
            {"range": rng(text, 0, len(text.encode())), "newText": formatted}]})
    elif method == "textDocument/codeAction":
        uri = params["textDocument"]["uri"]
        prefix = f"{TAG}: " if TAG else ""
        send({"jsonrpc": "2.0", "id": mid, "result": [
            {"title": prefix + "Add header", "data": {"uri": uri, "by": TAG}},
            {"title": "Touch file", "command": {"title": "Touch file", "command": "mock.touch", "arguments": [uri]}},
        ]})
    elif method == "codeAction/resolve":
        uri = params["data"]["uri"]
        zero = {"line": 0, "character": 0}
        by = (params["data"].get("by") or "") + ("" if TAG == (params["data"].get("by") or "") else " (asked of the wrong server)")
        params["edit"] = {"changes": {uri: [{"range": {"start": zero, "end": zero}, "newText": f"// header {by}".rstrip() + "\n"}]}}
        send({"jsonrpc": "2.0", "id": mid, "result": params})
    elif method == "tsserver/response":
        note("tsserver", params)
    elif method == "workspace/executeCommand" and params.get("command") == "typescript.tsserverRequest":
        # What the TypeScript server does with a question passed on to it.
        send({"jsonrpc": "2.0", "id": mid, "result": {
            "body": {"asked": params["arguments"][0], "about": params["arguments"][1]}}})
    elif method == "workspace/executeCommand":
        uri = params["arguments"][0]
        zero = {"line": 0, "character": 0}
        send({"jsonrpc": "2.0", "id": 9001, "method": "workspace/applyEdit", "params": {
            "edit": {"changes": {uri: [{"range": {"start": zero, "end": zero}, "newText": "// touched\n"}]}}}})
        send({"jsonrpc": "2.0", "id": mid, "result": None})
    elif method == "textDocument/signatureHelp":
        uri = params["textDocument"]["uri"]
        text = docs[uri]
        line = text.split("\n")[params["position"]["line"]].encode()[: params["position"]["character"]].decode()
        active = line[line.rfind("(") + 1:].count(",")
        send({"jsonrpc": "2.0", "id": mid, "result": {
            "signatures": [{"label": "helper(a: i32, b: i32)",
                            "parameters": [{"label": [7, 13]}, {"label": "b: i32"}]}],
            "activeSignature": 0, "activeParameter": active}})
    elif method == "shutdown":
        send({"jsonrpc": "2.0", "id": mid, "result": None})
    elif method == "exit":
        break
    elif mid is not None:
        send({"jsonrpc": "2.0", "id": mid, "result": None})
