// Runs before a plugin's script. Defines `solder`, then takes away the
// engine's own modules: a plugin talks to the editor and to nothing else.
//
// A request is a line on the output that starts with \x01; the editor's
// answer is the next line on the input. A handler that throws reports it
// with a line that starts with \x02.
(() => {
  const input = globalThis.std.in;
  const output = globalThis.std.out;
  for (const name of ["std", "os", "bjson", "scriptArgs"]) {
    delete globalThis[name];
  }

  const handlers = Object.create(null);
  const commands = Object.create(null);

  const ask = (request) => {
    output.puts("\x01" + JSON.stringify(request) + "\n");
    output.flush();
    const reply = JSON.parse(input.getline());
    if ("Err" in reply) throw new Error(reply.Err);
    return reply.Ok;
  };

  const log = (...parts) => {
    ask({ call: "log", text: parts.map(String).join(" ") });
  };

  globalThis.solder = Object.freeze({
    on(event, handler) {
      (handlers[event] ??= []).push(handler);
    },
    command(id, handler) {
      commands[id] = handler;
    },
    status(text) {
      ask({ call: "status", text: String(text) });
    },
    log,
    editor() {
      const file = ask({ call: "editor_text" });
      return {
        path: file.path,
        language: file.language,
        text: file.text,
        selectionStart: file.selection_start,
        selectionEnd: file.selection_end,
      };
    },
    edit(path, start, end, text) {
      ask({ call: "edit", path, start, end, text: String(text) });
    },
    readFile(path) {
      return ask({ call: "read_file", path: String(path) });
    },
    http(request) {
      const reply = ask({
        call: "http",
        method: request.method ?? "GET",
        url: request.url,
        headers: Object.entries(request.headers ?? {}),
        body: request.body ?? null,
      });
      return {
        status: reply.status,
        headers: Object.fromEntries(reply.headers),
        body: reply.body,
      };
    },
  });
  globalThis.console = Object.freeze({ log, info: log, warn: log, error: log, debug: log });

  // Reads events until the editor stops the plugin. Each read with nothing
  // waiting pauses the engine, which is how the editor knows an event is
  // done; awaiting between events lets the script's promises run.
  globalThis.__solder_run = async () => {
    delete globalThis.__solder_run;
    for (;;) {
      const line = input.getline();
      if (line === null) return;
      try {
        const event = JSON.parse(line);
        if (event.event === "command") {
          await commands[event.id]?.(event);
        } else {
          for (const handler of handlers[event.event] ?? []) await handler(event);
        }
      } catch (error) {
        const where = String(error?.stack ?? "").split("\n")[0].trim();
        output.puts("\x02" + String(error) + (where ? " " + where : "") + "\n");
        output.flush();
      }
    }
  };
})();
