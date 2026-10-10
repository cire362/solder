'use strict';
// The parts of VS Code's API an extension reaches the editor through: its
// window (messages, lists to pick from, the status bar, output) and its
// workspace (folders, files, settings, open documents).
//
// What an extension reads without waiting (the settings, the folders, the
// documents) is kept here, sent by the editor once and then as it changes.
// What needs the user or changes something is asked of the editor.

const fs = require('fs');
const path = require('path');
const { classes, enums, glob } = require('./types');
const { toRange, plainSelection } = require('./documents');

const { Disposable, EventEmitter, Uri, Range, CancellationTokenSource, FileSystemError } = classes;

module.exports = function build(core) {
  const { request, notify, docs } = core;
  // What the editor last said: the folders, the settings, the theme.
  const state = core.state;
  const foldersChanged = new EventEmitter();
  const configurationChanged = new EventEmitter();
  const themeChanged = new EventEmitter();
  const never = new EventEmitter();

  // ------------------------------------------------------------- settings

  // The value under `key` (`a.b.c`) of what the editor sent.
  function at(root, key) {
    let value = root;
    for (const part of key ? key.split('.') : []) {
      if (value === null || typeof value !== 'object') return undefined;
      value = value[part];
    }
    return value;
  }
  function copy(value) {
    return value === undefined ? undefined : JSON.parse(JSON.stringify(value));
  }
  function getConfiguration(section) {
    const prefix = section ? `${section}.` : '';
    const own = at(state.configuration, section || '');
    const config = own && typeof own === 'object' && !Array.isArray(own) ? copy(own) : {};
    const methods = {
      get: (key, fallback) => {
        const value = at(state.configuration, prefix + key);
        return value === undefined ? fallback : copy(value);
      },
      has: (key) => at(state.configuration, prefix + key) !== undefined,
      inspect: (key) => {
        const value = at(state.configuration, prefix + key);
        return value === undefined
          ? undefined
          : { key: prefix + key, defaultValue: copy(at(state.defaults, prefix + key)), globalValue: copy(value) };
      },
      update: (key, value) => request('updateConfiguration', { key: prefix + key, value: core.plain(value) }),
    };
    for (const [name, method] of Object.entries(methods)) {
      Object.defineProperty(config, name, { value: method, enumerable: false });
    }
    return config;
  }
  function configure({ configuration, defaults }) {
    const before = state.configuration;
    state.configuration = configuration || {};
    state.defaults = defaults || state.defaults || {};
    configurationChanged.fire({
      affectsConfiguration: (section) =>
        JSON.stringify(at(before, section)) !== JSON.stringify(at(state.configuration, section)),
    });
  }

  // -------------------------------------------------------------- folders

  function folders() {
    return state.folders.map((folder, index) => ({
      uri: Uri.file(folder),
      name: path.basename(folder),
      index,
    }));
  }
  function folderOf(uri) {
    const file = uri.fsPath;
    return folders()
      .filter((folder) => file === folder.uri.fsPath || file.startsWith(folder.uri.fsPath + '/'))
      .sort((a, b) => b.uri.fsPath.length - a.uri.fsPath.length)[0];
  }
  function setFolders({ folders: now }) {
    const before = folders();
    state.folders = now || [];
    const after = folders();
    const added = after.filter((a) => !before.some((b) => b.uri.fsPath === a.uri.fsPath));
    const removed = before.filter((b) => !after.some((a) => a.uri.fsPath === b.uri.fsPath));
    if (added.length || removed.length) foldersChanged.fire({ added, removed });
  }

  // Files under the folders whose path from the folder matches `include`.
  async function findFiles(include, exclude, maxResults) {
    const wanted = glob(typeof include === 'string' ? include : include.pattern);
    const roots = typeof include === 'string' ? state.folders : [include.base];
    const left = exclude ? glob(typeof exclude === 'string' ? exclude : exclude.pattern) : undefined;
    const found = [];
    const limit = maxResults || 10000;
    for (const root of roots) {
      const pending = [''];
      while (pending.length && found.length < limit) {
        const dir = pending.pop();
        let entries;
        try {
          entries = await fs.promises.readdir(path.join(root, dir), { withFileTypes: true });
        } catch {
          continue;
        }
        for (const entry of entries) {
          const relative = dir ? `${dir}/${entry.name}` : entry.name;
          if (left && left.test(relative)) continue;
          if (entry.isDirectory()) {
            // Never what no project means by its files.
            if (entry.name !== '.git' && entry.name !== 'node_modules') pending.push(relative);
          } else if (wanted.test(relative) && found.length < limit) {
            found.push(Uri.file(path.join(root, relative)));
          }
        }
      }
    }
    return found;
  }

  // Files as VS Code's `workspace.fs` gives them. The code of an extension
  // reaches files with Node anyway; this is the same reach under the names
  // extensions written for remote folders use.
  function failed(error, uri) {
    const codes = { ENOENT: 'FileNotFound', EEXIST: 'FileExists', ENOTDIR: 'FileNotADirectory', EISDIR: 'FileIsADirectory', EACCES: 'NoPermissions', EPERM: 'NoPermissions' };
    return new FileSystemError(uri, codes[error.code] || 'Unknown');
  }
  function kind(stat) {
    const type = stat.isDirectory() ? enums.FileType.Directory : stat.isFile() ? enums.FileType.File : enums.FileType.Unknown;
    return stat.isSymbolicLink() ? type | enums.FileType.SymbolicLink : type;
  }
  const files = {
    isWritableFileSystem: (scheme) => (scheme === 'file' ? true : undefined),
    async stat(uri) {
      try {
        const stat = await fs.promises.stat(uri.fsPath);
        return { type: kind(stat), ctime: stat.ctimeMs, mtime: stat.mtimeMs, size: stat.size };
      } catch (error) {
        throw failed(error, uri);
      }
    },
    async readDirectory(uri) {
      try {
        const entries = await fs.promises.readdir(uri.fsPath, { withFileTypes: true });
        return entries.map((entry) => [entry.name, kind(entry)]);
      } catch (error) {
        throw failed(error, uri);
      }
    },
    async readFile(uri) {
      try {
        return new Uint8Array(await fs.promises.readFile(uri.fsPath));
      } catch (error) {
        throw failed(error, uri);
      }
    },
    async writeFile(uri, content) {
      try {
        await fs.promises.writeFile(uri.fsPath, content);
      } catch (error) {
        throw failed(error, uri);
      }
    },
    async createDirectory(uri) {
      await fs.promises.mkdir(uri.fsPath, { recursive: true });
    },
    async delete(uri, options) {
      try {
        await fs.promises.rm(uri.fsPath, { recursive: !!(options && options.recursive) });
      } catch (error) {
        throw failed(error, uri);
      }
    },
    async rename(from, to, options) {
      if (!(options && options.overwrite) && fs.existsSync(to.fsPath)) throw FileSystemError.FileExists(to);
      await fs.promises.rename(from.fsPath, to.fsPath);
    },
    async copy(from, to, options) {
      if (!(options && options.overwrite) && fs.existsSync(to.fsPath)) throw FileSystemError.FileExists(to);
      await fs.promises.cp(from.fsPath, to.fsPath, { recursive: true });
    },
  };

  // ---------------------------------------------------------------- edits

  // Changes to texts go to the editor, which has the files that are open;
  // files made, renamed and deleted are done here first.
  async function applyEdit(edit) {
    for (const file of edit._files || []) {
      try {
        if (file.kind === 'create') {
          if (!fs.existsSync(file.uri.fsPath) || file.options.overwrite) {
            await fs.promises.mkdir(path.dirname(file.uri.fsPath), { recursive: true });
            await fs.promises.writeFile(file.uri.fsPath, file.options.contents || '');
          }
        } else if (file.kind === 'delete') {
          await fs.promises.rm(file.uri.fsPath, { recursive: !!file.options.recursive, force: !!file.options.ignoreIfNotExists });
        } else {
          await fs.promises.rename(file.uri.fsPath, file.newUri.fsPath);
        }
      } catch (error) {
        core.say('error', [error.message]);
        return false;
      }
    }
    return core.applyEdits(edit.entries());
  }
  core.applyEdits = (entries) => {
    const changes = {};
    for (const [uri, edits] of entries) {
      if (!edits.length) continue;
      changes[uri.toString()] = edits.map((edit) => ({ range: toRange(edit.range).toJSON(), newText: edit.newText }));
    }
    if (!Object.keys(changes).length) return Promise.resolve(true);
    return request('applyEdit', { changes }).then((applied) => applied !== false);
  };

  async function openTextDocument(target) {
    if (target && typeof target === 'object' && !Uri.isUri(target)) {
      throw new Error('A document that is no file cannot be opened here yet');
    }
    const uri = typeof target === 'string' ? Uri.file(target) : target;
    return docs.get(uri) || docs.read(uri);
  }

  const workspace = core.namespace('workspace', {
    get workspaceFolders() {
      const all = folders();
      return all.length ? all : undefined;
    },
    get rootPath() {
      return state.folders[0];
    },
    get name() {
      return state.folders[0] ? path.basename(state.folders[0]) : undefined;
    },
    workspaceFile: undefined,
    isTrusted: true,
    onDidGrantWorkspaceTrust: never.event,
    onDidChangeWorkspaceFolders: foldersChanged.event,
    getWorkspaceFolder: (uri) => folderOf(uri),
    asRelativePath(target, includeFolder) {
      const file = typeof target === 'string' ? target : target.fsPath;
      const folder = folderOf(Uri.file(file));
      if (!folder) return file;
      const relative = path.posix.relative(folder.uri.fsPath, file);
      return includeFolder && state.folders.length > 1 ? `${folder.name}/${relative}` : relative;
    },
    getConfiguration,
    onDidChangeConfiguration: configurationChanged.event,
    get textDocuments() {
      return docs.all;
    },
    openTextDocument,
    onDidOpenTextDocument: docs.opened.event,
    onDidCloseTextDocument: docs.closed.event,
    onDidChangeTextDocument: docs.changed.event,
    onDidSaveTextDocument: docs.saved.event,
    onWillSaveTextDocument: never.event,
    onDidCreateFiles: never.event,
    onDidDeleteFiles: never.event,
    onDidRenameFiles: never.event,
    onWillCreateFiles: never.event,
    onWillDeleteFiles: never.event,
    onWillRenameFiles: never.event,
    applyEdit,
    saveAll: () => request('saveAll', {}),
    save: (uri) => request('save', { uri: uri.toString() }).then(() => uri),
    findFiles,
    fs: files,
  });

  // --------------------------------------------------------------- window

  function titles(items) {
    return items.map((item) => (typeof item === 'string' ? item : item.title));
  }
  // A message for the user. With answers to choose from, it waits for one.
  function message(level) {
    return async (text, ...rest) => {
      const options = rest.length && rest[0] && typeof rest[0] === 'object' && !('title' in rest[0]) ? rest.shift() : {};
      const chosen = await request('message', {
        level,
        text: String(text),
        detail: options.detail,
        modal: !!options.modal,
        items: titles(rest),
      });
      return typeof chosen === 'number' ? rest[chosen] : undefined;
    };
  }

  async function showQuickPick(items, options = {}) {
    const all = (await items) || [];
    // Lines that only divide the list are not for choosing.
    const rows = all.filter((item) => typeof item === 'string' || item.kind !== enums.QuickPickItemKind.Separator);
    const chosen = await request('pick', {
      title: options.title,
      placeholder: options.placeHolder,
      items: rows.map((item) =>
        typeof item === 'string'
          ? { label: item }
          : { label: String(item.label), description: item.description, detail: item.detail },
      ),
    });
    if (typeof chosen !== 'number') return undefined;
    if (options.onDidSelectItem) options.onDidSelectItem(rows[chosen]);
    // One at a time: a list where several are ticked is not here yet.
    return options.canPickMany ? [rows[chosen]] : rows[chosen];
  }

  async function showInputBox(options = {}) {
    let value = options.value || '';
    let prompt = options.prompt;
    for (;;) {
      const typed = await request('input', {
        title: options.title,
        prompt,
        value,
        placeholder: options.placeHolder,
        password: !!options.password,
      });
      if (typeof typed !== 'string') return undefined;
      const wrong = options.validateInput ? await options.validateInput(typed) : undefined;
      if (!wrong) return typed;
      // Asked again with what is wrong, and what was typed still there.
      prompt = typeof wrong === 'string' ? wrong : wrong.message;
      value = typed;
    }
  }

  // The same two as things an extension builds and shows itself.
  function createQuickPick() {
    const accepted = new EventEmitter();
    const selected = new EventEmitter();
    const hidden = new EventEmitter();
    const typed = new EventEmitter();
    const pick = {
      items: [], selectedItems: [], activeItems: [], value: '', placeholder: undefined, title: undefined,
      busy: false, enabled: true, canSelectMany: false, matchOnDescription: false, matchOnDetail: false,
      ignoreFocusOut: false, buttons: [],
      onDidAccept: accepted.event,
      onDidChangeSelection: selected.event,
      onDidChangeActive: never.event,
      onDidChangeValue: typed.event,
      onDidTriggerButton: never.event,
      onDidTriggerItemButton: never.event,
      onDidHide: hidden.event,
      show() {
        showQuickPick(pick.items, { placeHolder: pick.placeholder, title: pick.title }).then((item) => {
          if (item !== undefined) {
            pick.selectedItems = [item];
            pick.activeItems = [item];
            selected.fire([item]);
            accepted.fire();
          }
          hidden.fire();
        });
      },
      hide() {},
      dispose() {},
    };
    return pick;
  }
  function createInputBox() {
    const accepted = new EventEmitter();
    const hidden = new EventEmitter();
    const typed = new EventEmitter();
    const box = {
      value: '', placeholder: undefined, prompt: undefined, title: undefined, password: false,
      busy: false, enabled: true, ignoreFocusOut: false, validationMessage: undefined, buttons: [],
      onDidAccept: accepted.event,
      onDidChangeValue: typed.event,
      onDidTriggerButton: never.event,
      onDidHide: hidden.event,
      show() {
        showInputBox({ value: box.value, prompt: box.prompt, placeHolder: box.placeholder, title: box.title, password: box.password }).then((value) => {
          if (value !== undefined) {
            box.value = value;
            typed.fire(value);
            accepted.fire();
          }
          hidden.fire();
        });
      },
      hide() {},
      dispose() {},
    };
    return box;
  }

  // An item of the status bar: what it says is sent when it changes, once
  // for all that changed in one go.
  let nextItem = 1;
  function createStatusBarItem(a, b, c) {
    const [alignment, priority] = typeof a === 'string' ? [b, c] : [a, b];
    const id = nextItem++;
    const shown = { text: '', tooltip: undefined, command: undefined, visible: false, name: undefined };
    let sending = false;
    const send = () => {
      if (sending) return;
      sending = true;
      queueMicrotask(() => {
        sending = false;
        const command = shown.command && typeof shown.command === 'object' ? shown.command.command : shown.command;
        const tooltip = shown.tooltip && typeof shown.tooltip === 'object' ? shown.tooltip.value : shown.tooltip;
        notify('status', {
          id,
          text: String(shown.text || ''),
          tooltip,
          command,
          args: shown.command && typeof shown.command === 'object' ? core.plain(shown.command.arguments) : undefined,
          visible: shown.visible,
          right: alignment === enums.StatusBarAlignment.Right,
          priority: priority || 0,
        });
      });
    };
    const item = {
      id: typeof a === 'string' ? a : `${core.extensionId}.${id}`,
      alignment: alignment || enums.StatusBarAlignment.Left,
      priority,
      show() {
        shown.visible = true;
        send();
      },
      hide() {
        shown.visible = false;
        send();
      },
      dispose() {
        notify('status', { id, gone: true });
      },
    };
    for (const key of ['text', 'tooltip', 'command', 'name', 'color', 'backgroundColor', 'accessibilityInformation']) {
      Object.defineProperty(item, key, {
        enumerable: true,
        get: () => shown[key],
        set: (value) => {
          shown[key] = value;
          send();
        },
      });
    }
    return item;
  }
  function setStatusBarMessage(text, hide) {
    const item = createStatusBarItem();
    item.text = text;
    item.show();
    const done = new Disposable(() => item.dispose());
    if (typeof hide === 'number') setTimeout(() => done.dispose(), hide);
    else if (hide && typeof hide.then === 'function') hide.then(() => done.dispose(), () => done.dispose());
    return done;
  }

  // A named place an extension writes what it does. The editor keeps the
  // text and opens it when asked.
  function createOutputChannel(name, options) {
    const log = !!(options && typeof options === 'object' && options.log);
    const write = (text) => notify('output', { channel: name, text });
    const channel = {
      name,
      append: (text) => write(String(text)),
      appendLine: (text) => write(`${text}\n`),
      replace(text) {
        notify('output', { channel: name, clear: true });
        write(String(text));
      },
      clear: () => notify('output', { channel: name, clear: true }),
      show: () => notify('output', { channel: name, show: true }),
      hide() {},
      dispose: () => notify('output', { channel: name, gone: true }),
    };
    if (log) {
      const line = (level) => (text, ...args) => {
        const more = args.length ? ' ' + args.map((arg) => (typeof arg === 'string' ? arg : JSON.stringify(arg))).join(' ') : '';
        const now = new Date().toISOString().replace('T', ' ').slice(0, 23);
        write(`${now} [${level}] ${text instanceof Error ? text.stack : text}${more}\n`);
      };
      Object.assign(channel, {
        logLevel: enums.LogLevel.Info,
        onDidChangeLogLevel: never.event,
        trace: line('trace'),
        debug: line('debug'),
        info: line('info'),
        warn: line('warning'),
        error: line('error'),
      });
    }
    return channel;
  }

  // Work that takes a while, said in the status bar while it runs.
  let nextProgress = 1;
  async function withProgress(options, task) {
    const id = nextProgress++;
    const title = options && options.title ? String(options.title) : '';
    notify('progress', { id, text: title });
    const source = new CancellationTokenSource();
    const progress = {
      report(value) {
        if (value && value.message) notify('progress', { id, text: title ? `${title}: ${value.message}` : String(value.message) });
      },
    };
    try {
      return await task(progress, source.token);
    } finally {
      notify('progress', { id, done: true });
      source.dispose();
    }
  }

  async function showTextDocument(target, options) {
    const uri = Uri.isUri(target) ? target : target.uri;
    const selection = options && typeof options === 'object' && options.selection
      ? plainSelection({ anchor: options.selection.start, active: options.selection.end })
      : undefined;
    await request('show', { uri: uri.toString(), selection });
    // The editor says the file is in front on its own time, which may be
    // a moment after it answered.
    const front = () => docs.active && docs.active.document.uri.toString() === uri.toString();
    if (front()) return docs.active;
    return new Promise((resolve, reject) => {
      const waiting = docs.activeChanged.event(() => {
        if (!front()) return;
        clearTimeout(late);
        waiting.dispose();
        resolve(docs.active);
      });
      const late = setTimeout(() => {
        waiting.dispose();
        reject(new Error(`Could not show ${uri.fsPath}`));
      }, 10000);
    });
  }

  const window = core.namespace('window', {
    get activeTextEditor() {
      return docs.active;
    },
    get visibleTextEditors() {
      return docs.active ? [docs.active] : [];
    },
    onDidChangeActiveTextEditor: docs.activeChanged.event,
    onDidChangeVisibleTextEditors: never.event,
    onDidChangeTextEditorSelection: docs.selectionChanged.event,
    onDidChangeTextEditorVisibleRanges: never.event,
    onDidChangeTextEditorOptions: never.event,
    onDidChangeTextEditorViewColumn: never.event,
    onDidChangeWindowState: never.event,
    state: { focused: true, active: true },
    get activeColorTheme() {
      return { kind: state.dark === false ? enums.ColorThemeKind.Light : enums.ColorThemeKind.Dark };
    },
    onDidChangeActiveColorTheme: themeChanged.event,
    showInformationMessage: message('info'),
    showWarningMessage: message('warning'),
    showErrorMessage: message('error'),
    showQuickPick,
    showInputBox,
    createQuickPick,
    createInputBox,
    createStatusBarItem,
    setStatusBarMessage,
    createOutputChannel,
    withProgress,
    showTextDocument,
  });

  // ------------------------------------------------------------------ env

  const env = core.namespace('env', {
    appName: 'Solder',
    appHost: 'desktop',
    appRoot: core.extensionDir,
    language: 'en',
    machineId: 'solder',
    sessionId: 'solder',
    uiKind: enums.UIKind.Desktop,
    uriScheme: 'solder',
    remoteName: undefined,
    shell: process.env.SHELL || '/bin/sh',
    logLevel: enums.LogLevel.Info,
    onDidChangeLogLevel: never.event,
    isNewAppInstall: false,
    isTelemetryEnabled: false,
    onDidChangeTelemetryEnabled: never.event,
    clipboard: {
      readText: () => request('clipboardRead', {}).then((text) => text || ''),
      writeText: (text) => request('clipboardWrite', { text: String(text) }),
    },
    openExternal: (uri) => request('openExternal', { url: uri.toString() }).then((opened) => opened !== false),
    asExternalUri: async (uri) => uri,
  });

  // Texts of the extension in the user's language. Solder has one.
  const l10n = {
    bundle: undefined,
    uri: undefined,
    t(text, ...args) {
      const [template, values] = text && typeof text === 'object'
        ? [text.message, text.args || []]
        : [text, args.length === 1 && args[0] && typeof args[0] === 'object' ? args[0] : args];
      return String(template).replace(/\{(\w+)\}/g, (all, key) => (values[key] === undefined ? all : String(values[key])));
    },
  };

  // What the editor says without being asked.
  const told = {
    folders: setFolders,
    configuration: configure,
    opened: (params) => void docs.open(params),
    closed: (params) => docs.close(params),
    changed: (params) => docs.change(params),
    saved: (params) => docs.save(params),
    active: (params) => docs.front(params),
    theme({ dark }) {
      if (state.dark === dark) return;
      state.dark = dark;
      themeChanged.fire(window.activeColorTheme);
    },
    // Everything at once, before the extension is loaded.
    init(params) {
      state.folders = params.folders || [];
      state.configuration = params.configuration || {};
      state.defaults = params.defaults || {};
      state.dark = params.dark;
      for (const document of params.documents || []) docs.open(document);
      if (params.active) docs.front(params.active);
    },
  };

  return { window, workspace, env, l10n, told, Range };
};
