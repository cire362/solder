'use strict';
// The editor's open files as an extension sees them. The editor sends each
// file once and then every change to it, so the text here is the editor's,
// and an extension reads it without asking and without waiting.

const fs = require('fs');
const { classes, enums } = require('./types');

const { EventEmitter, Uri, Position, Range, Selection } = classes;

// What files of an ending are called, for one read from disk: the editor
// says it itself for the ones it has open.
const LANGUAGES = {
  rs: 'rust', ts: 'typescript', tsx: 'typescriptreact', js: 'javascript', mjs: 'javascript',
  cjs: 'javascript', jsx: 'javascriptreact', json: 'json', jsonc: 'jsonc', css: 'css',
  scss: 'scss', less: 'less', html: 'html', htm: 'html', vue: 'vue', go: 'go', py: 'python',
  c: 'c', h: 'c', cpp: 'cpp', cc: 'cpp', hpp: 'cpp', md: 'markdown', yaml: 'yaml', yml: 'yaml',
  sh: 'shellscript', bash: 'shellscript', zsh: 'shellscript', toml: 'toml', xml: 'xml',
  java: 'java', kt: 'kotlin', rb: 'ruby', php: 'php', swift: 'swift', sql: 'sql', lua: 'lua',
};

class TextDocument {
  constructor(uri, languageId, version, text, live) {
    this.uri = uri;
    this.languageId = languageId;
    this.version = version;
    this.isDirty = false;
    this.isClosed = false;
    this.isUntitled = uri.scheme === 'untitled';
    this.eol = enums.EndOfLine.LF;
    this._text = text;
    this._starts = undefined;
    // Whether the editor keeps it up to date: one read from disk is not.
    this._live = live;
  }
  get fileName() {
    return this.uri.fsPath;
  }
  // Where each line starts. Found again only after a change.
  get _lines() {
    if (!this._starts) {
      const starts = [0];
      for (let at = this._text.indexOf('\n'); at >= 0; at = this._text.indexOf('\n', at + 1)) {
        starts.push(at + 1);
      }
      this._starts = starts;
    }
    return this._starts;
  }
  get lineCount() {
    return this._lines.length;
  }
  _end(line) {
    const starts = this._lines;
    return line + 1 < starts.length ? starts[line + 1] - 1 : this._text.length;
  }
  validatePosition(position) {
    if (position.line < 0) return new Position(0, 0);
    const line = Math.min(position.line, this.lineCount - 1);
    // Past the last line is the end of the text.
    if (position.line > line) return new Position(line, this._end(line) - this._lines[line]);
    const length = this._end(line) - this._lines[line];
    return new Position(line, Math.min(Math.max(position.character, 0), length));
  }
  validateRange(range) {
    return new Range(this.validatePosition(range.start), this.validatePosition(range.end));
  }
  offsetAt(position) {
    const valid = this.validatePosition(position);
    return this._lines[valid.line] + valid.character;
  }
  positionAt(offset) {
    const starts = this._lines;
    const at = Math.min(Math.max(offset, 0), this._text.length);
    let low = 0;
    let high = starts.length - 1;
    while (low < high) {
      const middle = (low + high + 1) >> 1;
      if (starts[middle] <= at) low = middle;
      else high = middle - 1;
    }
    return new Position(low, at - starts[low]);
  }
  getText(range) {
    if (!range) return this._text;
    return this._text.slice(this.offsetAt(range.start), this.offsetAt(range.end));
  }
  lineAt(lineOrPosition) {
    const line = typeof lineOrPosition === 'number' ? lineOrPosition : lineOrPosition.line;
    if (line < 0 || line >= this.lineCount) throw new Error('Illegal value for `line`');
    const text = this._text.slice(this._lines[line], this._end(line));
    const first = /^\s*/.exec(text)[0].length;
    const range = new Range(line, 0, line, text.length);
    return {
      lineNumber: line,
      text,
      range,
      rangeIncludingLineBreak: line + 1 < this.lineCount ? new Range(line, 0, line + 1, 0) : range,
      firstNonWhitespaceCharacterIndex: first,
      isEmptyOrWhitespace: first === text.length,
    };
  }
  getWordRangeAtPosition(position, regex) {
    const valid = this.validatePosition(position);
    const text = this.lineAt(valid.line).text;
    const source = regex ? regex.source : '[\\p{L}\\p{N}_$]+';
    const flags = regex ? regex.flags.replace(/[gy]/g, '') + 'g' : 'gu';
    const word = new RegExp(source, flags);
    for (let match = word.exec(text); match; match = word.exec(text)) {
      if (!match[0].length) {
        word.lastIndex += 1;
        continue;
      }
      const end = match.index + match[0].length;
      if (match.index <= valid.character && valid.character <= end) {
        return new Range(valid.line, match.index, valid.line, end);
      }
      if (match.index > valid.character) break;
    }
    return undefined;
  }
  // One change of the editor's, in the text as it was just before it.
  _change(range, text) {
    const start = this.offsetAt(range.start);
    const end = this.offsetAt(range.end);
    this._text = this._text.slice(0, start) + text + this._text.slice(end);
    this._starts = undefined;
    return { range, rangeOffset: start, rangeLength: end - start, text };
  }
}

// What an editor shows of a document. There is one for the file in front.
class TextEditor {
  constructor(document, selections, core) {
    this.document = document;
    this.selections = selections;
    this.viewColumn = enums.ViewColumn.One;
    this.options = { tabSize: core.tabSize(), insertSpaces: true, cursorStyle: 1, lineNumbers: 1 };
    this._core = core;
  }
  get selection() {
    return this.selections[0];
  }
  set selection(selection) {
    this.selections = [selection];
    this._core.notify('select', { uri: this.document.uri.toString(), selections: [plainSelection(selection)] });
  }
  get visibleRanges() {
    return [new Range(0, 0, Math.max(this.document.lineCount - 1, 0), 0)];
  }
  // Changes gathered by `callback` and made as one.
  edit(callback) {
    const edits = [];
    callback({
      replace: (where, text) => edits.push({ range: toRange(where), newText: text }),
      insert: (position, text) => edits.push({ range: new Range(position, position), newText: text }),
      delete: (where) => edits.push({ range: toRange(where), newText: '' }),
      setEndOfLine() {},
    });
    if (!edits.length) return Promise.resolve(true);
    return this._core.applyEdits([[this.document.uri, edits]]);
  }
  // A snippet's text with its places taken out: the cursor does not go
  // from one to the next here.
  insertSnippet(snippet, where) {
    const text = String(snippet.value === undefined ? snippet : snippet.value)
      .replace(/\$\{\d+:([^}]*)\}/g, '$1')
      .replace(/\$\{\d+\|([^,|}]*)[^}]*\}/g, '$1')
      .replace(/\$\d+|\$\{\d+\}/g, '');
    const places = where === undefined ? this.selections : [].concat(where);
    const edits = places.map((place) => ({ range: toRange(place), newText: text }));
    return this._core.applyEdits([[this.document.uri, edits]]);
  }
  revealRange(range) {
    this._core.notify('reveal', { uri: this.document.uri.toString(), range: toRange(range).toJSON() });
  }
  setDecorations() {
    this._core.missing('TextEditor.setDecorations');
  }
  show() {}
  hide() {}
}

function toRange(where) {
  return Range.isRange(where) ? new Range(where.start, where.end) : new Range(where, where);
}

function plainSelection(selection) {
  return {
    anchor: { line: selection.anchor.line, character: selection.anchor.character },
    active: { line: selection.active.line, character: selection.active.character },
  };
}

// Every document the editor has open, and the one in front.
class Documents {
  constructor(core) {
    this._core = core;
    this._all = new Map();
    this.active = undefined;
    this.opened = new EventEmitter();
    this.closed = new EventEmitter();
    this.changed = new EventEmitter();
    this.saved = new EventEmitter();
    this.activeChanged = new EventEmitter();
    this.selectionChanged = new EventEmitter();
  }
  get all() {
    return [...this._all.values()];
  }
  get(uri) {
    return this._all.get(typeof uri === 'string' ? uri : uri.toString());
  }
  open({ uri, languageId, version, text, dirty }) {
    const document = new TextDocument(Uri.parse(uri), languageId, version, text, true);
    document.isDirty = !!dirty;
    document.save = () => this._core.request('save', { uri });
    this._all.set(document.uri.toString(), document);
    this.opened.fire(document);
    return document;
  }
  close({ uri }) {
    const document = this.get(Uri.parse(uri));
    if (!document) return;
    this._all.delete(document.uri.toString());
    document.isClosed = true;
    if (this.active && this.active.document === document) this.front({ uri: null });
    this.closed.fire(document);
  }
  change({ uri, version, changes, dirty }) {
    const document = this.get(Uri.parse(uri));
    if (!document) return;
    const made = changes.map(({ range, text }) => {
      const where = new Range(range.start.line, range.start.character, range.end.line, range.end.character);
      return document._change(where, text);
    });
    document.version = version;
    document.isDirty = dirty !== false;
    this.changed.fire({ document, contentChanges: made, reason: undefined });
  }
  save({ uri }) {
    const document = this.get(Uri.parse(uri));
    if (!document) return;
    document.isDirty = false;
    this.saved.fire(document);
  }
  // Another file came to the front, or the selection in it moved.
  front({ uri, selections }) {
    const document = uri ? this.get(Uri.parse(uri)) : undefined;
    const chosen = (selections || []).map(
      ({ anchor, active }) => new Selection(anchor.line, anchor.character, active.line, active.character),
    );
    if (!chosen.length) chosen.push(new Selection(0, 0, 0, 0));
    if (!document) {
      if (this.active) {
        this.active = undefined;
        this.activeChanged.fire(undefined);
      }
      return;
    }
    if (!this.active || this.active.document !== document) {
      this.active = new TextEditor(document, chosen, this._core);
      this.activeChanged.fire(this.active);
      return;
    }
    const before = JSON.stringify(this.active.selections.map(plainSelection));
    this.active.selections = chosen;
    if (before !== JSON.stringify(chosen.map(plainSelection))) {
      this.selectionChanged.fire({ textEditor: this.active, selections: chosen, kind: undefined });
    }
  }
  // A file that is not open in the editor, as it is on disk now.
  read(uri) {
    const text = fs.readFileSync(uri.fsPath, 'utf8');
    const ending = /\.([^./]+)$/.exec(uri.path);
    const languageId = (ending && LANGUAGES[ending[1].toLowerCase()]) || 'plaintext';
    const document = new TextDocument(uri, languageId, 1, text.replace(/\r\n/g, '\n'), false);
    document.save = () => Promise.resolve(true);
    return document;
  }
}

module.exports = { Documents, TextDocument, TextEditor, toRange, plainSelection };
