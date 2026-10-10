'use strict';
// Notebooks: files that are a list of cells, some of them code that an
// extension runs and shows the results of.
//
// The extension does the two things that are its own: it reads the file
// into cells and writes them back (a serializer), and it runs the code
// cells (a controller). The editor shows the cells, each an editor of its
// own, and what the runs put out. So this side keeps the notebook as the
// extension sees it, and says to the editor what a run puts out.

const fs = require('fs');
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
// form that is neither, only what it is and how large.
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
      };
    }),
  }));
}

module.exports = function buildNotebooks(core) {
  const serializers = new Map();
  const controllers = [];
  // The notebooks the editor has open, by where their file is.
  const open = new Map();
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
  function makeNotebook(uri, type, data) {
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
        await asked['notebook.save']({ uri: uri.toString() });
        core.notify('notebook.saved', { uri: uri.toString() });
        return true;
      },
    };
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
      one.outputs = cell.outputs;
      one.metadata = cell.metadata;
      one.executionSummary = cell.executionSummary;
      return one;
    }));
    data.metadata = notebook.metadata;
    return data;
  }
  function describe(cell) {
    return {
      handle: cell._handle,
      code: cell.kind === NotebookCellKind.Code,
      language: cell.document.languageId,
      value: cell.document.getText(),
      outputs: plainOutputs(cell.outputs),
      order: cell.executionSummary && cell.executionSummary.executionOrder,
    };
  }
  function touched(notebook, event) {
    notebook.version += 1;
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
      core.notify('notebook.outputs', { uri, handle: target._handle, outputs: plainOutputs(target.outputs) });
      touched(notebook, { cellChanges: [{ cell: target, outputs: target.outputs }] });
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
        began = time || Date.now();
        notebook._running.add(source);
        core.notify('notebook.run', { uri, handle: cell._handle, running: true });
      },
      end(success, time) {
        notebook._running.delete(source);
        cell.executionSummary = { executionOrder: order, success, timing: began ? { startTime: began, endTime: time || Date.now() } : undefined };
        core.notify('notebook.run', { uri, handle: cell._handle, running: false, failed: success === false, order });
        touched(notebook, { cellChanges: [{ cell, executionSummary: cell.executionSummary }] });
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
    // A file read into cells by the extension that knows its kind.
    'notebook.open': async ({ type, uri }) => {
      const serializer = serializers.get(type);
      if (!serializer) throw new Error(`The extension reads no notebooks of the kind ${type}`);
      const where = Uri.parse(uri);
      const known = open.get(where.toString());
      if (known) return { cells: known._cells.map(describe), runner: runner(type) && String(runner(type).label) };
      const bytes = await fs.promises.readFile(where.fsPath);
      const data = await serializer.deserializeNotebook(bytes, never);
      const notebook = makeNotebook(where, type, data);
      open.set(where.toString(), notebook);
      opened.fire(notebook);
      const controller = runner(type);
      if (controller) controller._selected.fire({ notebook, selected: true });
      return { cells: notebook._cells.map(describe), runner: controller && String(controller.label || controller.id) };
    },
    // Written back by the same extension, from the cells as they are now.
    'notebook.save': async ({ uri }) => {
      const notebook = found(uri);
      const serializer = serializers.get(notebook.notebookType);
      if (!serializer) throw new Error(`The extension writes no notebooks of the kind ${notebook.notebookType}`);
      const bytes = await serializer.serializeNotebook(dataOf(notebook), never);
      await fs.promises.writeFile(notebook.uri.fsPath, Buffer.from(bytes));
      notebook.isDirty = false;
      saved.fire(notebook);
      return true;
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
    // The text of a cell as it is now.
    'notebook.cell'({ uri, handle, value }) {
      const notebook = open.get(key(uri));
      const cell = notebook && notebook._cells.find((one) => one._handle === handle);
      if (!cell) return;
      const document = cell.document;
      const whole = new Range(0, 0, document.lineCount, 0);
      const was = document.getText().length;
      document._text = String(value);
      document._starts = undefined;
      document.version += 1;
      notebook.isDirty = true;
      core.docs.changed.fire({ document, contentChanges: [{ range: whole, rangeOffset: 0, rangeLength: was, text: document._text }], reason: undefined });
      touched(notebook, { cellChanges: [{ cell, document }] });
    },
    // The cells in their order, after one was added, removed or moved. A
    // number the notebook does not know yet is a new cell.
    'notebook.cells'({ uri, cells }) {
      const notebook = open.get(key(uri));
      if (!notebook) return;
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
    },
    'notebook.close'({ uri }) {
      const notebook = open.get(key(uri));
      if (!notebook) return;
      open.delete(key(uri));
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
