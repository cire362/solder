'use strict';
// Notebooks: files that are a list of cells, some of them code that an
// extension runs and shows the results of.
//
// The extension does the two things that are its own: it reads the file
// into cells and writes them back (a serializer), and it runs the code
// cells (a controller). The editor shows the cells, each an editor of its
// own, and what the runs put out. So this side keeps the notebook as the
// extension sees it, and says to the editor what a run puts out.

const fileSave = require('./file-save');
const crypto = require('crypto');
const path = require('path');
const { classes } = require('./types');
const { TextDocument } = require('./documents');

const { EventEmitter, Uri, Disposable, Range, CancellationTokenSource } = classes;

const NotebookCellKind = { Markup: 1, Code: 2 };
const STDOUT = 'application/vnd.code.notebook.stdout';
const STDERR = 'application/vnd.code.notebook.stderr';
const ERROR = 'application/vnd.code.notebook.error';

class NotebookRange {
  constructor(start, end) {
    this.start = Math.min(start, end);
    this.end = Math.max(start, end);
  }
  get isEmpty() {
    return this.start === this.end;
  }
  with({ start = this.start, end = this.end } = {}) {
    return new NotebookRange(start, end);
  }
}
class NotebookCellOutputItem {
  static text(value, mime = 'text/plain') {
    return new NotebookCellOutputItem(Buffer.from(String(value)), mime);
  }
  static json(value, mime = 'text/x-json') {
    return new NotebookCellOutputItem(Buffer.from(JSON.stringify(value, undefined, '\t')), mime);
  }
  static stdout(value) {
    return NotebookCellOutputItem.text(value, STDOUT);
  }
  static stderr(value) {
    return NotebookCellOutputItem.text(value, STDERR);
  }
  static error(value) {
    const { name, message, stack } = value || {};
    return NotebookCellOutputItem.json({ name, message, stack }, ERROR);
  }
  constructor(data, mime) {
    this.data = data;
    this.mime = mime;
  }
}
class NotebookCellOutput {
  constructor(items, metadata) {
    this.items = items || [];
    this.metadata = metadata;
  }
}
class NotebookCellData {
  constructor(kind, value, languageId) {
    this.kind = kind;
    this.value = value;
    this.languageId = languageId;
  }
}
class NotebookData {
  constructor(cells) {
    this.cells = cells || [];
  }
}

const notebookClasses = { NotebookRange, NotebookCellOutputItem, NotebookCellOutput, NotebookCellData, NotebookData };
const notebookEnums = {
  NotebookCellKind,
  NotebookControllerAffinity: { Default: 1, Preferred: 2 },
  NotebookEditorRevealType: { Default: 0, InCenter: 1, InCenterIfOutsideViewport: 2, AtTop: 3 },
  NotebookCellStatusBarAlignment: { Left: 1, Right: 2 },
};

// What an output is to the editor, which draws words and pictures: each
// of its forms with the text it has, or with the picture as it is. Of a
// rich form, the bounded bytes that a declared renderer can draw.
const PICTURES = ['image/png', 'image/jpeg', 'image/gif', 'image/webp'];
const PICTURE_LIMIT = 8 << 20;
function wordsOf(item) {
  const data = Buffer.from(item.data || []);
  if (item.mime === ERROR) {
    try {
      const { name, message, stack } = JSON.parse(data.toString());
      return stack || [name, message].filter(Boolean).join(': ');
    } catch {
      return data.toString();
    }
  }
  const worded = /^text\//.test(item.mime) || /json$/.test(item.mime) || item.mime === STDOUT || item.mime === STDERR;
  return worded ? data.toString() : undefined;
}
function plainOutputs(outputs) {
  return (outputs || []).map((output) => ({
    items: (output.items || []).map((item) => {
      const size = item.data ? item.data.length : 0;
      const picture = PICTURES.includes(item.mime) && size <= PICTURE_LIMIT;
      return {
        mime: String(item.mime),
        size,
        text: wordsOf(item),
        picture: picture ? Buffer.from(item.data).toString('base64') : undefined,
        data: size <= PICTURE_LIMIT && !picture && wordsOf(item) === undefined ? Buffer.from(item.data || []).toString('base64') : undefined,
      };
    }),
  }));
}

// Recovery uses NotebookData, never the serializer's file format. Binary
// output items keep all their bytes, including forms the editor cannot draw.
function packedData(data) {
  for (const cell of data.cells) {
    for (const output of cell.outputs) {
      for (const item of output.items) item.data = Buffer.from(item.data).toString('base64');
    }
  }
  return JSON.parse(JSON.stringify(data));
}

function packedRevision(revision) {
  return { ...revision, ...Object.fromEntries(['size', 'mtime', 'dev', 'ino'].map((field) => [field, revision[field].toString()])) };
}

function unpackSnapshot(snapshot, type) {
  const invalid = () => { throw new Error('Invalid or unsupported notebook recovery snapshot'); };
  const object = (value) => value !== null && typeof value === 'object' && !Array.isArray(value);
  const metadata = (value) => value === undefined || object(value);
  if (!object(snapshot) || snapshot.version !== 1 || snapshot.type !== type
    || !Number.isSafeInteger(snapshot.version_at_capture) || snapshot.version_at_capture < 1
    || !object(snapshot.revision) || !object(snapshot.data) || !Array.isArray(snapshot.data.cells)
    || !metadata(snapshot.data.metadata)) invalid();
  const revision = { ...snapshot.revision };
  if (typeof revision.resolved !== 'string' || !path.isAbsolute(revision.resolved)
    || typeof revision.sha256 !== 'string' || !/^[a-f0-9]{64}$/.test(revision.sha256)) invalid();
  for (const field of ['size', 'mtime', 'dev', 'ino']) {
    const value = revision[field];
    if (typeof value !== 'string' || !(field === 'mtime' ? /^-?(0|[1-9]\d*)$/ : /^(0|[1-9]\d*)$/).test(value)) invalid();
    revision[field] = BigInt(value);
  }
  // Clone metadata and summaries too, so a queued restore owns its request.
  const data = JSON.parse(JSON.stringify(snapshot.data));
  for (const cell of data.cells) {
    if (!object(cell) || ![NotebookCellKind.Markup, NotebookCellKind.Code].includes(cell.kind)
      || typeof cell.value !== 'string' || typeof cell.languageId !== 'string'
      || !metadata(cell.metadata) || !Array.isArray(cell.outputs)) invalid();
    const summary = cell.executionSummary;
    if (summary !== undefined && (!object(summary)
      || (summary.executionOrder !== undefined && (!Number.isSafeInteger(summary.executionOrder) || summary.executionOrder < 0))
      || (summary.success !== undefined && typeof summary.success !== 'boolean')
      || (summary.timing !== undefined && (!object(summary.timing)
        || !Number.isFinite(summary.timing.startTime) || !Number.isFinite(summary.timing.endTime))))) invalid();
    for (const output of cell.outputs) {
      if (!object(output) || !Array.isArray(output.items) || !metadata(output.metadata)) invalid();
      for (const item of output.items) {
        if (!object(item) || typeof item.mime !== 'string' || typeof item.data !== 'string'
          || item.data.length % 4 !== 0 || !/^[A-Za-z0-9+/]*={0,2}$/.test(item.data)) invalid();
        const bytes = Buffer.from(item.data, 'base64');
        if (bytes.toString('base64') !== item.data) invalid();
        item.data = bytes;
      }
    }
  }
  return { data, revision, version: snapshot.version_at_capture };
}

module.exports = function buildNotebooks(core) {
  const serializers = new Map();
  const messaging = new Map();
  const rendererPages = new Map();
  const controllers = [];
  // The notebooks the editor has open, by where their file is.
  const open = new Map();
  const openings = new Map();
  const files = new WeakMap();
  const closedViews = new Set();
  const copyTokens = new Map();
  const COPY_TOKEN_LIMIT = 128;
  const COPY_TOKEN_LIFETIME = 5 * 60 * 1000;
  const opened = new EventEmitter();
  const closed = new EventEmitter();
  const changed = new EventEmitter();
  const saved = new EventEmitter();
  const frontChanged = new EventEmitter();
  const never = new CancellationTokenSource().token;
  let front;

  function key(uri) {
    return Uri.parse(String(uri)).toString();
  }
  function found(uri) {
    const notebook = open.get(key(uri));
    if (!notebook) throw new Error('This notebook is not open');
    return notebook;
  }

  // A cell, with its text as a document like any other the extension
  // reads. The editor names it by a number that stays its own wherever
  // the cell is moved.
  function makeCell(notebook, handle, { kind, language, value, outputs, metadata, summary }) {
    const uri = Uri.from({ scheme: 'vscode-notebook-cell', path: notebook.uri.path, fragment: `c${handle}` });
    const document = new TextDocument(uri, language || 'plaintext', 1, String(value || ''), true);
    document.notebook = notebook;
    const cell = {
      _handle: handle,
      notebook,
      kind: kind === NotebookCellKind.Markup ? NotebookCellKind.Markup : NotebookCellKind.Code,
      document,
      metadata: metadata || {},
      outputs: outputs || [],
      executionSummary: summary,
      get index() {
        return notebook._cells.indexOf(cell);
      },
    };
    core.docs._all.set(uri.toString(), document);
    core.docs.opened.fire(document);
    return cell;
  }
  function dropCell(cell) {
    core.docs._all.delete(cell.document.uri.toString());
    cell.document.isClosed = true;
    core.docs.closed.fire(cell.document);
  }
  function makeNotebook(uri, type, data, revision) {
    const notebook = {
      uri,
      notebookType: type,
      version: 1,
      isDirty: false,
      isUntitled: false,
      isClosed: false,
      metadata: (data && data.metadata) || {},
      _cells: [],
      _running: new Set(),
      get cellCount() {
        return notebook._cells.length;
      },
      cellAt(index) {
        return notebook._cells[Math.max(0, Math.min(index, notebook._cells.length - 1))];
      },
      getCells(range) {
        return range ? notebook._cells.slice(range.start, range.end) : [...notebook._cells];
      },
      // Saved by the extension itself: the editor is told it is as on disk.
      async save() {
        const client_tag = notebook._clientTag;
        const version = notebook.version;
        await saveNotebook(notebook);
        if (!notebook.isDirty) core.notify('notebook.saved', { uri: uri.toString(), client_tag, version });
        return true;
      },
    };
    files.set(notebook, { path: uri.fsPath, revision, saves: Promise.resolve() });
    notebook._cells = ((data && data.cells) || []).map((cell, handle) => makeCell(notebook, handle, {
      kind: cell.kind,
      language: cell.languageId,
      value: cell.value,
      outputs: cell.outputs,
      metadata: cell.metadata,
      summary: cell.executionSummary,
    }));
    return notebook;
  }
  // The notebook as a serializer is given it: what is in the cells now.
  function dataOf(notebook) {
    const data = new NotebookData(notebook._cells.map((cell) => {
      const one = new NotebookCellData(cell.kind, cell.document.getText(), cell.document.languageId);
      one.outputs = cell.outputs.map((output) => new NotebookCellOutput(
        output.items.map((item) => new NotebookCellOutputItem(Buffer.from(item.data), item.mime)),
        structuredClone(output.metadata),
      ));
      one.metadata = structuredClone(cell.metadata);
      one.executionSummary = structuredClone(cell.executionSummary);
      return one;
    }));
    data.metadata = structuredClone(notebook.metadata);
    return data;
  }
  function inSaveQueue(notebook, work) {
    const file = files.get(notebook);
    const pending = file.saves.then(work);
    // A refused request keeps the original revision and leaves the queue
    // available to later saves, recovery snapshots and copies.
    file.saves = pending.catch(() => {});
    return pending;
  }
  async function saveNotebook(notebook) {
    const serializer = serializers.get(notebook.notebookType);
    if (!serializer) throw new Error(`The extension writes no notebooks of the kind ${notebook.notebookType}`);
    // A queued save owns the cells requested, not whatever a preceding
    // serializer or write leaves in the notebook when its turn comes.
    const data = dataOf(notebook);
    const version = notebook.version;
    const file = files.get(notebook);
    return inSaveQueue(notebook, async () => {
      const bytes = Buffer.from(await serializer.serializeNotebook(data, never));
      file.revision = await fileSave.save(file.path, file.revision, bytes);
      if (notebook.version === version) notebook.isDirty = false;
      saved.fire(notebook);
      return true;
    });
  }
  function describe(cell) {
    return {
      handle: cell._handle,
      code: cell.kind === NotebookCellKind.Code,
      language: cell.document.languageId,
      value: cell.document.getText(),
      outputs: plainOutputs(cell.outputs),
      order: cell.executionSummary && cell.executionSummary.executionOrder,
      ...(cell.executionSummary?.success === false ? { failed: true } : {}),
    };
  }
  function touched(notebook, event) {
    notebook.version += 1;
    notebook.isDirty = true;
    changed.fire({ notebook, metadata: undefined, contentChanges: [], cellChanges: [], ...event });
  }
  function editorOf(notebook, selected = 0) {
    const at = Math.max(0, Math.min(selected, notebook._cells.length));
    const selection = new NotebookRange(at, Math.min(at + 1, notebook._cells.length));
    return {
      notebook,
      selection,
      selections: [selection],
      visibleRanges: [new NotebookRange(0, notebook._cells.length)],
      viewColumn: 1,
      revealRange() {},
    };
  }

  // What runs the cells of a kind of notebook: the first the extension
  // made for it. The editor is told there is one, to offer the running.
  function runner(type) {
    return controllers.find((controller) => controller.notebookType === type);
  }
  function described(notebook) {
    const controller = runner(notebook.notebookType);
    return { dirty: notebook.isDirty, client_tag: notebook._clientTag, version: notebook.version, cells: notebook._cells.map(describe), runner: controller && String(controller.label || controller.id) };
  }
  function announce() {
    core.notify('notebook.runners', {
      runners: controllers.map((controller) => ({ type: controller.notebookType, label: String(controller.label || controller.id) })),
    });
  }
  // A run of one cell, as the extension holds it: it says when the run
  // starts and ends, and puts what comes out under the cell.
  function execution(cell) {
    const notebook = cell.notebook;
    const uri = notebook.uri.toString();
    const source = new CancellationTokenSource();
    let order;
    let began;
    const put = (target = cell) => {
      if (notebook.isClosed) return;
      touched(notebook, { cellChanges: [{ cell: target, outputs: target.outputs }] });
      core.notify('notebook.outputs', { uri, handle: target._handle, outputs: plainOutputs(target.outputs), version: notebook.version });
    };
    const list = (value) => [].concat(value || []);
    const run = {
      cell,
      token: source.token,
      get executionOrder() {
        return order;
      },
      set executionOrder(value) {
        order = value;
      },
      start(time) {
        if (notebook.isClosed) return;
        began = time || Date.now();
        notebook._running.add(source);
        core.notify('notebook.run', { uri, handle: cell._handle, running: true });
      },
      end(success, time) {
        if (notebook.isClosed) return;
        notebook._running.delete(source);
        cell.executionSummary = { executionOrder: order, success, timing: began ? { startTime: began, endTime: time || Date.now() } : undefined };
        touched(notebook, { cellChanges: [{ cell, executionSummary: cell.executionSummary }] });
        core.notify('notebook.run', { uri, handle: cell._handle, running: false, failed: success === false, order, version: notebook.version });
      },
      async clearOutput(target = cell) {
        target.outputs = [];
        put(target);
      },
      async replaceOutput(out, target = cell) {
        target.outputs = list(out);
        put(target);
      },
      async appendOutput(out, target = cell) {
        target.outputs = [...target.outputs, ...list(out)];
        put(target);
      },
      async replaceOutputItems(items, output) {
        output.items = list(items);
        put(notebook._cells.find((one) => one.outputs.includes(output)) || cell);
      },
      async appendOutputItems(items, output) {
        output.items = [...output.items, ...list(items)];
        put(notebook._cells.find((one) => one.outputs.includes(output)) || cell);
      },
    };
    return run;
  }

  const notebooks = core.namespace('notebooks', {
    createRendererMessaging(id) {
      if (messaging.has(id)) return messaging.get(id);
      const received = new EventEmitter();
      const channel = {
        onDidReceiveMessage: received.event,
        _received: received,
        async postMessage(message, editor) {
          const targets = [...rendererPages].filter(([, page]) => page.renderer === id
            && (!editor || page.uri === editor.notebook.uri.toString()));
          const sent = await Promise.all(targets.map(([id]) => core.request('webview.post', { id, message: core.plain(message) })));
          return sent.some(Boolean);
        },
      };
      messaging.set(id, channel);
      return channel;
    },
    createNotebookController(id, notebookType, label, handler) {
      const selected = new EventEmitter();
      const received = new EventEmitter();
      const controller = {
        id,
        notebookType,
        label,
        description: undefined,
        detail: undefined,
        supportedLanguages: undefined,
        supportsExecutionOrder: false,
        executeHandler: handler,
        interruptHandler: undefined,
        onDidChangeSelectedNotebooks: selected.event,
        onDidReceiveMessage: received.event,
        createNotebookCellExecution: (cell) => execution(cell),
        updateNotebookAffinity() {},
        // What is drawn of an output is not a page, so nothing hears it.
        postMessage: async () => false,
        asWebviewUri: (uri) => uri,
        dispose() {
          const at = controllers.indexOf(controller);
          if (at >= 0) controllers.splice(at, 1);
          announce();
        },
      };
      controllers.push(controller);
      announce();
      // The notebooks of its kind that are open already are its to run.
      for (const notebook of open.values()) {
        if (notebook.notebookType === notebookType && runner(notebookType) === controller) selected.fire({ notebook, selected: true });
      }
      controller._selected = selected;
      return controller;
    },
  });

  const workspaceMembers = {
    registerNotebookSerializer(type, serializer) {
      serializers.set(type, serializer);
      return new Disposable(() => {
        if (serializers.get(type) === serializer) serializers.delete(type);
      });
    },
    get notebookDocuments() {
      return [...open.values()];
    },
    onDidOpenNotebookDocument: opened.event,
    onDidCloseNotebookDocument: closed.event,
    onDidChangeNotebookDocument: changed.event,
    onDidSaveNotebookDocument: saved.event,
  };
  const windowMembers = {
    get activeNotebookEditor() {
      return front;
    },
    get visibleNotebookEditors() {
      return front ? [front] : [];
    },
    onDidChangeActiveNotebookEditor: frontChanged.event,
    onDidChangeVisibleNotebookEditors: new EventEmitter().event,
    onDidChangeNotebookEditorSelection: new EventEmitter().event,
    onDidChangeNotebookEditorVisibleRanges: new EventEmitter().event,
  };

  const asked = {
    'notebook.renderer.activate': async () => true,
    // A file read into cells by the extension that knows its kind.
    'notebook.open': async ({ type, uri, lifecycle }) => {
      if (lifecycle && closedViews.has(lifecycle)) throw new Error('The notebook view was closed');
      const serializer = serializers.get(type);
      if (!serializer) throw new Error(`The extension reads no notebooks of the kind ${type}`);
      const where = Uri.parse(uri);
      const name = where.toString();
      const known = open.get(name);
      if (known) return described(known);
      const pending = openings.get(name);
      if (pending) return pending.promise;
      const opening = { lifecycle };
      openings.set(name, opening);
      opening.promise = (async () => {
        const source = await fileSave.read(where.fsPath);
        const data = await serializer.deserializeNotebook(source.bytes, never);
        // Restore and close invalidate this opening, even if the restored
        // notebook was closed again before deserialization finished.
        if (openings.get(name) !== opening) {
          const arrived = open.get(name);
          if (arrived) return described(arrived);
          throw new Error('The notebook opening was superseded');
        }
        const notebook = makeNotebook(where, type, data, source.revision);
        notebook._view = lifecycle;
        open.set(name, notebook);
        opened.fire(notebook);
        const controller = runner(type);
        if (controller) controller._selected.fire({ notebook, selected: true });
        return described(notebook);
      })().finally(() => {
        if (openings.get(name) === opening) openings.delete(name);
      });
      return opening.promise;
    },
    'notebook.snapshot': async ({ uri }) => {
      const notebook = found(uri);
      const data = packedData(dataOf(notebook));
      const version = notebook.version;
      return inSaveQueue(notebook, () => ({
        version: 1,
        type: notebook.notebookType,
        // Only our preceding successful saves may advance this stamp.
        revision: packedRevision(files.get(notebook).revision),
        data,
        version_at_capture: version,
      }));
    },
    'notebook.restore': async ({ uri, type, snapshot, lifecycle }) => {
      if (!serializers.has(type)) throw new Error(`The extension reads no notebooks of the kind ${type}`);
      const recovered = unpackSnapshot(snapshot, type);
      const where = Uri.parse(uri);
      const known = open.get(where.toString());
      const replace = () => {
        if (lifecycle && closedViews.has(lifecycle)) throw new Error('The notebook view was closed');
        const current = open.get(where.toString());
        const pending = openings.get(where.toString());
        if (lifecycle && ((current?._view && current._view !== lifecycle)
          || (pending?.lifecycle && pending.lifecycle !== lifecycle))) {
          throw new Error('Cannot restore over another notebook view');
        }
        if ((known && (known.isClosed || current !== known)) || (current && current.isDirty)) {
          throw new Error('Cannot restore over a closed or changed notebook');
        }
        openings.delete(where.toString());
        const selected = front && front.notebook === current ? front.selection.start : undefined;
        if (current) {
          current.isClosed = true;
          for (const source of [...current._running]) source.cancel();
          current._running.clear();
          for (const cell of current._cells) dropCell(cell);
          closed.fire(current);
        }
        const notebook = makeNotebook(where, type, recovered.data, recovered.revision);
        notebook._view = lifecycle;
        notebook.version = Math.max(recovered.version, current ? current.version + 1 : 1);
        notebook.isDirty = true;
        open.set(where.toString(), notebook);
        opened.fire(notebook);
        if (selected !== undefined) {
          front = editorOf(notebook, selected);
          frontChanged.fire(front);
        }
        const controller = runner(type);
        if (controller) controller._selected.fire({ notebook, selected: true });
        return described(notebook);
      };
      if (known && known.isDirty) throw new Error('Cannot restore over an open dirty notebook');
      return known ? inSaveQueue(known, replace) : replace();
    },
    'notebook.export.prepare': async ({ uri, target }) => {
      const notebook = found(uri);
      const file = files.get(notebook);
      const where = Uri.parse(target);
      if (where.scheme !== 'file' || where.authority || where.query || where.fragment) throw new Error('A notebook copy needs a local file');
      const expected = await fileSave.prepareCopy(file.path, file.revision, where.fsPath);
      const now = Date.now();
      for (const [token, entry] of copyTokens) if (entry.expires <= now) copyTokens.delete(token);
      while (copyTokens.size >= COPY_TOKEN_LIMIT) copyTokens.delete(copyTokens.keys().next().value);
      const token = crypto.randomBytes(32).toString('hex');
      copyTokens.set(token, { notebook, target: where.toString(), expected, expires: now + COPY_TOKEN_LIFETIME });
      return token;
    },
    'notebook.export': async ({ uri, target, token }) => {
      const prepared = copyTokens.get(token);
      copyTokens.delete(token);
      const notebook = found(uri);
      const where = Uri.parse(target);
      if (!prepared || prepared.notebook !== notebook || prepared.target !== where.toString()
        || prepared.expires <= Date.now()) throw new Error('Invalid or expired notebook copy token');
      const serializer = serializers.get(notebook.notebookType);
      if (!serializer) throw new Error(`The extension writes no notebooks of the kind ${notebook.notebookType}`);
      const data = dataOf(notebook);
      const file = files.get(notebook);
      return inSaveQueue(notebook, async () => {
        const bytes = Buffer.from(await serializer.serializeNotebook(data, never));
        await fileSave.writeCopy(file.path, file.revision, where.fsPath, prepared.expected, bytes);
        return true;
      });
    },
    // Written back by the same extension, from the cells as they are now.
    'notebook.save': async ({ uri }) => {
      return saveNotebook(found(uri));
    },
    // The cells to run, given to what the extension made to run them.
    'notebook.execute': async ({ uri, handles }) => {
      const notebook = found(uri);
      const controller = runner(notebook.notebookType);
      if (!controller || !controller.executeHandler) throw new Error('No extension is here to run the cells of this notebook');
      const cells = notebook._cells.filter((cell) => handles.includes(cell._handle) && cell.kind === NotebookCellKind.Code);
      await controller.executeHandler.call(controller, cells, notebook, controller);
      return true;
    },
    'notebook.interrupt': async ({ uri }) => {
      const notebook = found(uri);
      const controller = runner(notebook.notebookType);
      if (controller && controller.interruptHandler) await controller.interruptHandler.call(controller, notebook);
      for (const source of [...notebook._running]) source.cancel();
      return true;
    },
  };

  const told = {
    'notebook.renderer.open'({ id, renderer, uri }) {
      rendererPages.set(id, { renderer, uri });
    },
    'notebook.renderer.message'({ renderer, uri, message }) {
      const notebook = open.get(key(uri));
      messaging.get(renderer)?._received.fire({ editor: notebook ? editorOf(notebook) : undefined, message });
    },
    'notebook.renderer.closed'({ id }) { rendererPages.delete(id); },
    // The text of a cell as it is now.
    'notebook.cell'({ uri, handle, value, client_tag }) {
      const notebook = open.get(key(uri));
      const cell = notebook && notebook._cells.find((one) => one._handle === handle);
      if (!cell) return;
      notebook._clientTag = client_tag;
      const document = cell.document;
      const whole = new Range(0, 0, document.lineCount, 0);
      const was = document.getText().length;
      document._text = String(value);
      document._starts = undefined;
      document.version += 1;
      notebook.isDirty = true;
      core.docs.changed.fire({ document, contentChanges: [{ range: whole, rangeOffset: 0, rangeLength: was, text: document._text }], reason: undefined });
      touched(notebook, { cellChanges: [{ cell, document }] });
      core.notify('notebook.version', { uri, version: notebook.version, client_tag });
    },
    // The cells in their order, after one was added, removed or moved. A
    // number the notebook does not know yet is a new cell.
    'notebook.cells'({ uri, cells, client_tag }) {
      const notebook = open.get(key(uri));
      if (!notebook) return;
      notebook._clientTag = client_tag;
      const before = notebook._cells;
      const after = cells.map((one) => before.find((cell) => cell._handle === one.handle)
        || makeCell(notebook, one.handle, { kind: one.code ? NotebookCellKind.Code : NotebookCellKind.Markup, language: one.language, value: one.value }));
      // What stayed where it was at both ends is not said to have changed.
      let head = 0;
      while (head < before.length && head < after.length && before[head] === after[head]) head += 1;
      let tail = 0;
      while (tail < before.length - head && tail < after.length - head && before[before.length - 1 - tail] === after[after.length - 1 - tail]) tail += 1;
      const removedCells = before.slice(head, before.length - tail);
      const addedCells = after.slice(head, after.length - tail);
      notebook._cells = after;
      notebook.isDirty = true;
      for (const cell of removedCells) if (!after.includes(cell)) dropCell(cell);
      touched(notebook, { contentChanges: [{ range: new NotebookRange(head, before.length - tail), removedCells, addedCells }] });
      core.notify('notebook.version', { uri, version: notebook.version, client_tag });
    },
    'notebook.close'({ uri, lifecycle }) {
      if (lifecycle) {
        closedViews.add(lifecycle);
        if (closedViews.size > 512) closedViews.delete(closedViews.values().next().value);
      }
      openings.delete(key(uri));
      const notebook = open.get(key(uri));
      if (!notebook) return;
      if (lifecycle && notebook._view && notebook._view !== lifecycle) return;
      open.delete(key(uri));
      for (const [token, entry] of copyTokens) if (entry.notebook === notebook) copyTokens.delete(token);
      for (const source of [...notebook._running]) source.cancel();
      for (const cell of notebook._cells) dropCell(cell);
      notebook.isClosed = true;
      if (front && front.notebook === notebook) {
        front = undefined;
        frontChanged.fire(front);
      }
      closed.fire(notebook);
    },
    // The notebook in front, and the cell the cursor is in.
    'notebook.front'({ uri, selected }) {
      const notebook = uri ? open.get(key(uri)) : undefined;
      front = notebook ? editorOf(notebook, selected) : undefined;
      frontChanged.fire(front);
    },
  };

  return { notebooks, workspaceMembers, windowMembers, asked, told, classes: notebookClasses, enums: notebookEnums };
};
