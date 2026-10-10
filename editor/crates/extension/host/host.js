'use strict';
// Solder's host for one VS Code extension. Started as
//   node host.js <extension folder> <storage folder>
// it talks to the editor on its input and output, one JSON message a line,
// loads the extension when the editor says an event it waits for happened,
// and gives it a `vscode` module of Solder's own.
//
// One process holds one extension: one that crashes or never returns takes
// nothing else with it, and the editor ends a process that stops answering.

const fs = require('fs');
const path = require('path');
const util = require('util');
const Module = require('module');

const [extensionDir, storageDir] = process.argv.slice(2);
const manifest = JSON.parse(fs.readFileSync(path.join(extensionDir, 'package.json'), 'utf8'));
const extensionId = `${manifest.publisher}.${manifest.name}`;

// ------------------------------------------------------------ the protocol

// What the extension prints is not the protocol: it goes to the editor as
// what it says, and only `send` writes to the real output.
const write = process.stdout.write.bind(process.stdout);
function send(message) {
  write(JSON.stringify(message) + '\n');
}
function notify(method, params) {
  send({ jsonrpc: '2.0', method, params });
}
let nextId = 1;
const waiting = new Map();
function request(method, params) {
  return new Promise((resolve, reject) => {
    const id = nextId++;
    waiting.set(id, { resolve, reject });
    send({ jsonrpc: '2.0', id, method, params });
  });
}
function say(level, args) {
  notify('log', { level, text: util.format(...args) });
}
process.stdout.write = (chunk, encoding, done) => {
  notify('log', { level: 'info', text: String(chunk).replace(/\n$/, '') });
  const after = typeof encoding === 'function' ? encoding : done;
  if (after) after();
  return true;
};
for (const level of ['log', 'info', 'debug', 'warn', 'error']) {
  console[level] = (...args) => say(level === 'log' ? 'info' : level, args);
}
// A mistake in the extension is its own: said, and the host goes on, as
// VS Code's does.
process.on('uncaughtException', (error) => say('error', [error && error.stack || error]));
process.on('unhandledRejection', (error) => say('error', [error && error.stack || error]));

// What can go over the wire of what an extension hands back.
function plain(value) {
  if (value === undefined) return null;
  try {
    return JSON.parse(JSON.stringify(value));
  } catch {
    return null;
  }
}

// ------------------------------------------------------- the `vscode` module

class Disposable {
  constructor(dispose) {
    this._dispose = dispose;
  }
  static from(...disposables) {
    return new Disposable(() => disposables.forEach((d) => d && d.dispose && d.dispose()));
  }
  dispose() {
    const dispose = this._dispose;
    this._dispose = undefined;
    if (dispose) dispose();
  }
}

class EventEmitter {
  constructor() {
    this._listeners = new Set();
    this.event = (listener, thisArg, disposables) => {
      const entry = { listener, thisArg };
      this._listeners.add(entry);
      const disposable = new Disposable(() => this._listeners.delete(entry));
      if (Array.isArray(disposables)) disposables.push(disposable);
      return disposable;
    };
  }
  fire(value) {
    for (const { listener, thisArg } of [...this._listeners]) {
      try {
        listener.call(thisArg, value);
      } catch (error) {
        say('error', [error && error.stack || error]);
      }
    }
  }
  dispose() {
    this._listeners.clear();
  }
}

class Uri {
  constructor(scheme, authority, uriPath, query, fragment) {
    this.scheme = scheme || 'file';
    this.authority = authority || '';
    this.path = uriPath || '';
    this.query = query || '';
    this.fragment = fragment || '';
  }
  static file(filePath) {
    let normal = filePath.replace(/\\/g, '/');
    if (!normal.startsWith('/')) normal = '/' + normal;
    return new Uri('file', '', normal);
  }
  static parse(value) {
    const match = /^([a-zA-Z][\w+.-]*):(?:\/\/([^/?#]*))?([^?#]*)(?:\?([^#]*))?(?:#(.*))?$/.exec(value);
    if (!match) return Uri.file(value);
    return new Uri(match[1], match[2], decodeURIComponent(match[3] || ''), match[4], match[5]);
  }
  static from(parts) {
    return new Uri(parts.scheme, parts.authority, parts.path, parts.query, parts.fragment);
  }
  static joinPath(base, ...segments) {
    return base.with({ path: path.posix.join(base.path || '/', ...segments) });
  }
  static isUri(value) {
    return value instanceof Uri;
  }
  get fsPath() {
    return this.path;
  }
  with(change) {
    return new Uri(
      change.scheme !== undefined ? change.scheme : this.scheme,
      change.authority !== undefined ? change.authority : this.authority,
      change.path !== undefined ? change.path : this.path,
      change.query !== undefined ? change.query : this.query,
      change.fragment !== undefined ? change.fragment : this.fragment,
    );
  }
  toString() {
    const encoded = this.path.split('/').map(encodeURIComponent).join('/');
    const authority = this.authority || this.scheme === 'file' ? `//${this.authority}` : '';
    const query = this.query ? `?${this.query}` : '';
    const fragment = this.fragment ? `#${this.fragment}` : '';
    return `${this.scheme}:${authority}${encoded}${query}${fragment}`;
  }
  toJSON() {
    return { $uri: this.toString(), scheme: this.scheme, path: this.path, fsPath: this.fsPath };
  }
}

// The commands this extension registered, by name.
const commands = new Map();

// A part of the API that is not here yet. Asking for it does not stop the
// extension: it gets something it can dispose of, and the editor is told
// what was asked for, so the Extensions tab can say what does not work.
const asked = new Set();
function missing(name) {
  if (!asked.has(name)) {
    asked.add(name);
    notify('missing', { name });
  }
}
function namespace(name, members) {
  return new Proxy(members, {
    get(target, key) {
      if (key in target || typeof key === 'symbol') return target[key];
      missing(`${name}.${String(key)}`);
      return () => new Disposable(() => {});
    },
  });
}

const vscode = {
  version: '1.90.0',
  Disposable,
  EventEmitter,
  Uri,
  commands: namespace('commands', {
    registerCommand(id, handler, thisArg) {
      commands.set(id, { handler, thisArg });
      notify('command', { id, registered: true });
      return new Disposable(() => {
        commands.delete(id);
        notify('command', { id, registered: false });
      });
    },
    // One of its own runs here; any other is the editor's to find.
    async executeCommand(id, ...args) {
      const own = commands.get(id);
      if (own) return own.handler.apply(own.thisArg, args);
      return request('executeCommand', { id, args: plain(args) });
    },
    async getCommands() {
      return [...commands.keys()];
    },
  }),
  env: namespace('env', {
    appName: 'Solder',
    appHost: 'desktop',
    language: 'en',
    machineId: 'solder',
    sessionId: 'solder',
    uiKind: 1,
    isTelemetryEnabled: false,
    onDidChangeTelemetryEnabled: new EventEmitter().event,
  }),
  extensions: namespace('extensions', {
    all: [],
    getExtension(id) {
      return id.toLowerCase() === extensionId.toLowerCase() ? self : undefined;
    },
    onDidChange: new EventEmitter().event,
  }),
};
// What is not a namespace above is one that is not here yet at all.
const api = new Proxy(vscode, {
  get(target, key) {
    if (key in target || typeof key === 'symbol') return target[key];
    target[key] = namespace(String(key), {});
    return target[key];
  },
});

// An extension asks for `vscode` as for any module; this is the one it gets.
const load = Module._load;
Module._load = function (name, ...rest) {
  if (name === 'vscode') return api;
  return load.call(this, name, ...rest);
};

// --------------------------------------------------------- the extension

// What the extension remembers between runs, in a file of its own.
function memento(file) {
  let values = {};
  try {
    values = JSON.parse(fs.readFileSync(file, 'utf8'));
  } catch {
    // Nothing remembered yet.
  }
  return {
    keys: () => Object.keys(values),
    get: (key, fallback) => (key in values ? values[key] : fallback),
    update(key, value) {
      if (value === undefined) delete values[key];
      else values[key] = value;
      fs.mkdirSync(path.dirname(file), { recursive: true });
      fs.writeFileSync(file, JSON.stringify(values));
      return Promise.resolve();
    },
    setKeysForSync() {},
  };
}

const self = {
  id: extensionId,
  extensionPath: extensionDir,
  extensionUri: Uri.file(extensionDir),
  packageJSON: manifest,
  extensionKind: 1,
  isActive: false,
  exports: undefined,
  activate: () => Promise.resolve(self.exports),
};

const secrets = new Map();
const context = {
  subscriptions: [],
  extension: self,
  extensionPath: extensionDir,
  extensionUri: Uri.file(extensionDir),
  extensionMode: 1,
  globalState: memento(path.join(storageDir, 'global.json')),
  workspaceState: memento(path.join(storageDir, 'workspace.json')),
  secrets: {
    get: async (key) => secrets.get(key),
    store: async (key, value) => void secrets.set(key, value),
    delete: async (key) => void secrets.delete(key),
    onDidChange: new EventEmitter().event,
  },
  storagePath: path.join(storageDir, 'workspace'),
  globalStoragePath: path.join(storageDir, 'global'),
  logPath: path.join(storageDir, 'log'),
  storageUri: Uri.file(path.join(storageDir, 'workspace')),
  globalStorageUri: Uri.file(path.join(storageDir, 'global')),
  logUri: Uri.file(path.join(storageDir, 'log')),
  asAbsolutePath: (relative) => path.join(extensionDir, relative),
  environmentVariableCollection: { replace() {}, append() {}, prepend() {}, get() {}, delete() {}, clear() {}, forEach() {} },
};

let main;
let activating;
function activate() {
  if (!activating) {
    activating = (async () => {
      if (!manifest.main) return;
      main = require(path.resolve(extensionDir, manifest.main));
      if (main && typeof main.activate === 'function') {
        self.exports = await main.activate(context);
      }
      self.isActive = true;
    })();
  }
  return activating;
}

async function deactivate() {
  if (main && typeof main.deactivate === 'function') await main.deactivate();
  for (const disposable of context.subscriptions.splice(0)) {
    try {
      if (disposable && disposable.dispose) disposable.dispose();
    } catch (error) {
      say('error', [error && error.stack || error]);
    }
  }
}

// ---------------------------------------------------- what the editor asks

const handlers = {
  ping: async () => ({}),
  activate: async () => {
    await activate();
    return { commands: [...commands.keys()] };
  },
  deactivate,
  executeCommand: async ({ id, args }) => {
    const own = commands.get(id);
    if (!own) throw new Error(`No command ${id}`);
    return plain(await own.handler.apply(own.thisArg, args || []));
  },
};

async function handle(message) {
  if (message.method === undefined) {
    // An answer to something this side asked.
    const asker = waiting.get(message.id);
    if (!asker) return;
    waiting.delete(message.id);
    if (message.error) asker.reject(new Error(message.error.message || 'refused'));
    else asker.resolve(message.result);
    return;
  }
  const handler = handlers[message.method];
  if (message.id === undefined) {
    if (handler) await handler(message.params || {});
    return;
  }
  try {
    if (!handler) throw new Error(`The host does not do ${message.method}`);
    send({ jsonrpc: '2.0', id: message.id, result: plain(await handler(message.params || {})) });
  } catch (error) {
    const text = error && error.message ? error.message : String(error);
    send({ jsonrpc: '2.0', id: message.id, error: { code: -32000, message: text } });
  }
}

let pending = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', (chunk) => {
  pending += chunk;
  let end;
  while ((end = pending.indexOf('\n')) >= 0) {
    const line = pending.slice(0, end);
    pending = pending.slice(end + 1);
    if (!line.trim()) continue;
    let message;
    try {
      message = JSON.parse(line);
    } catch {
      continue;
    }
    handle(message).catch((error) => say('error', [error && error.stack || error]));
  }
});
// The editor closed its end: there is nobody left to work for.
process.stdin.on('end', () => process.exit(0));

module.exports = { handlers, vscode: api, request, notify, plain, context, commands };
notify('ready', { id: extensionId });
