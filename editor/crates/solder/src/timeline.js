// Solder's request timeline, loaded into the debugged Node program with
// `--require`. It records the HTTP requests the program serves and sends and
// the SQL it runs through pg and mysql2, each with the line of the program's
// own code that made it, as JSON lines appended to $SOLDER_TIMELINE. Child
// processes inherit NODE_OPTIONS and append to the same file.
"use strict";

(() => {
  const file = process.env.SOLDER_TIMELINE;
  if (!file || globalThis.__solderTimeline) return;
  globalThis.__solderTimeline = true;

  const fs = require("fs");
  const Module = require("module");
  const { AsyncLocalStorage } = require("async_hooks");
  const { performance } = require("perf_hooks");
  const { fileURLToPath } = require("url");

  let fd;
  try {
    fd = fs.openSync(file, "a");
  } catch {
    return;
  }
  // Call sites in compiled code (Next.js, TypeScript) point at the sources.
  if (typeof process.setSourceMapsEnabled === "function") {
    process.setSourceMapsEnabled(true);
  }

  // Appends are small and atomic, so processes sharing the file do not
  // interleave lines.
  function write(entry) {
    entry.pid = process.pid;
    try {
      fs.writeSync(fd, JSON.stringify(entry) + "\n");
    } catch {}
  }

  // The first frame in the program's own code.
  function site() {
    const holder = {};
    const limit = Error.stackTraceLimit;
    Error.stackTraceLimit = 50;
    Error.captureStackTrace(holder, site);
    Error.stackTraceLimit = limit;
    for (const frame of String(holder.stack).split("\n").slice(1)) {
      const m = /at (?:.*? \()?(.+?):(\d+):\d+\)?$/.exec(frame.trim());
      if (!m) continue;
      let path = m[1];
      if (path.startsWith("file://")) {
        try {
          path = fileURLToPath(path);
        } catch {
          continue;
        }
      }
      if (
        path === __filename ||
        path.startsWith("node:") ||
        path.includes("/node_modules/") ||
        path.includes("\\node_modules\\")
      ) {
        continue;
      }
      return { path, line: Number(m[2]) };
    }
    return {};
  }

  // Wall clock below a millisecond: a request and the call its handler
  // makes at once must not tie.
  const now = () => performance.timeOrigin + performance.now();

  // The served request a query or a call runs for.
  const request = new AsyncLocalStorage();
  // Stands for a request left out, along with the work done for it.
  const QUIET = Symbol("quiet");
  let served = 0;

  function begin(kind, label, extra) {
    const at = now();
    const where = site();
    const parent = request.getStore();
    if (parent === QUIET) return () => {};
    let done = false;
    return (fields) => {
      if (done) return;
      done = true;
      write({ kind, label, at, ms: now() - at, parent, ...where, ...extra, ...fields });
    };
  }

  function message(error) {
    return String((error && error.message) || error);
  }

  // ------------------------------------------------------------ served HTTP

  // A dev server's own assets and hot reload would bury the app's requests.
  const quiet = /^\/(_next\/|__nextjs|favicon\.ico)/;
  const http = require("http");
  const https = require("https");
  for (const Server of [http.Server, https.Server]) {
    const emit = Server.prototype.emit;
    Server.prototype.emit = function (event, req, res) {
      if (event !== "request" || !req || !res) {
        return emit.apply(this, arguments);
      }
      if (quiet.test(req.url || "")) {
        return request.run(QUIET, () => emit.apply(this, arguments));
      }
      const id = `${process.pid}:${++served}`;
      const at = now();
      res.once("close", () =>
        write({
          kind: "in",
          id,
          label: `${req.method} ${req.url}`,
          status: res.statusCode,
          at,
          ms: now() - at,
        }),
      );
      return request.run(id, () => emit.apply(this, arguments));
    };
  }

  // -------------------------------------------------------------- sent HTTP

  for (const mod of [http, https]) {
    for (const name of ["request", "get"]) {
      const original = mod[name];
      mod[name] = function () {
        const req = original.apply(this, arguments);
        try {
          // The Host header keeps a port that is not the default.
          const url = `${req.protocol}//${req.getHeader("host") || req.host}${req.path}`;
          const end = begin("out", `${req.method} ${url}`);
          req.once("response", (res) => res.once("close", () => end({ status: res.statusCode })));
          req.once("error", (e) => end({ error: message(e) }));
        } catch {}
        return req;
      };
    }
  }
  Module.syncBuiltinESMExports();

  // Until the response headers arrive.
  if (typeof globalThis.fetch === "function") {
    const original = globalThis.fetch;
    globalThis.fetch = function (input, init) {
      const method = (init && init.method) || (input && input.method) || "GET";
      const url = typeof input === "string" ? input : (input && (input.url || input.href)) || String(input);
      const end = begin("out", `${method.toUpperCase()} ${url}`);
      return original.apply(this, arguments).then(
        (res) => {
          end({ status: res.status });
          return res;
        },
        (e) => {
          end({ error: message(e) });
          throw e;
        },
      );
    };
  }

  // -------------------------------------------------------------------- SQL

  function sqlText(query) {
    const text = typeof query === "string" ? query : query && (query.text || query.sql);
    return typeof text === "string" ? text.replace(/\s+/g, " ").trim().slice(0, 2000) : null;
  }

  // A trailing callback is wrapped; otherwise `settle` watches the result.
  function wrapQuery(proto, name, db, settle) {
    const original = proto && proto[name];
    if (typeof original !== "function" || original.__solder) return;
    const wrapped = function () {
      const text = sqlText(arguments[0]);
      if (!text || typeof arguments[0].submit === "function") {
        return original.apply(this, arguments);
      }
      const end = begin("sql", text, { db });
      const args = Array.from(arguments);
      const last = args.length - 1;
      if (typeof args[last] === "function") {
        const callback = args[last];
        args[last] = function (error, result) {
          end(error ? { error: message(error) } : { rows: rowCount(result) });
          return callback.apply(this, arguments);
        };
        return original.apply(this, args);
      }
      const result = original.apply(this, args);
      settle(result, end);
      return result;
    };
    wrapped.__solder = true;
    proto[name] = wrapped;
  }

  function rowCount(result) {
    if (!result) return undefined;
    if (typeof result.rowCount === "number") return result.rowCount;
    if (Array.isArray(result)) return result.length;
    if (typeof result.affectedRows === "number") return result.affectedRows;
    return undefined;
  }

  // pg returns a promise without a callback.
  function patchPg(pg) {
    wrapQuery(pg && pg.Client && pg.Client.prototype, "query", "postgres", (result, end) => {
      if (result && typeof result.then === "function") {
        result.then(
          (r) => end({ rows: rowCount(r) }),
          (e) => end({ error: message(e) }),
        );
      }
    });
  }

  // mysql2 returns an emitter without a callback; only `end` is watched, as
  // listening for `error` would stop an unhandled one from throwing.
  function patchMysql(mysql) {
    const proto = mysql && mysql.Connection && mysql.Connection.prototype;
    for (const name of ["query", "execute"]) {
      wrapQuery(proto, name, "mysql", (result, end) => {
        if (result && typeof result.once === "function") result.once("end", () => end({}));
      });
    }
  }

  const load = Module._load;
  Module._load = function (name, parent) {
    const exports = load.apply(this, arguments);
    try {
      if (name === "pg") patchPg(exports);
      else if (name === "mysql2") patchMysql(exports);
      else if (name === "mysql2/promise") patchMysql(load.call(this, "mysql2", parent, false));
    } catch {}
    return exports;
  };
})();
