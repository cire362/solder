'use strict';
// The values of VS Code's API that are only data: positions, ranges, edits,
// and the small things everything else is built from. Nothing here talks
// to the editor.

const path = require('path');

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

// Where a listener's mistake is said; the host sets it.
const report = { error: () => {} };

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
        report.error(error);
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

// A place in a text: a line and a count of UTF-16 units in it, which is
// what a JavaScript string counts in too.
class Position {
  constructor(line, character) {
    if (line < 0 || character < 0) throw new Error('A position is not before the start');
    this.line = line;
    this.character = character;
  }
  static isPosition(value) {
    return value instanceof Position
      || (!!value && typeof value.line === 'number' && typeof value.character === 'number');
  }
  compareTo(other) {
    return this.line - other.line || this.character - other.character;
  }
  isBefore(other) { return this.compareTo(other) < 0; }
  isBeforeOrEqual(other) { return this.compareTo(other) <= 0; }
  isAfter(other) { return this.compareTo(other) > 0; }
  isAfterOrEqual(other) { return this.compareTo(other) >= 0; }
  isEqual(other) { return this.compareTo(other) === 0; }
  translate(lineDelta = 0, characterDelta = 0) {
    if (lineDelta && typeof lineDelta === 'object') {
      return this.translate(lineDelta.lineDelta || 0, lineDelta.characterDelta || 0);
    }
    return new Position(this.line + lineDelta, this.character + characterDelta);
  }
  with(line = this.line, character = this.character) {
    if (line && typeof line === 'object') {
      return this.with(line.line === undefined ? this.line : line.line, line.character);
    }
    return new Position(line, character);
  }
  toJSON() {
    return { line: this.line, character: this.character };
  }
}

class Range {
  constructor(a, b, c, d) {
    let start;
    let end;
    if (typeof a === 'number') {
      start = new Position(a, b);
      end = new Position(c, d);
    } else {
      start = new Position(a.line, a.character);
      end = new Position(b.line, b.character);
    }
    // A range reads forward, whichever way it was given.
    if (start.isAfter(end)) [start, end] = [end, start];
    this.start = start;
    this.end = end;
  }
  static isRange(value) {
    return value instanceof Range
      || (!!value && Position.isPosition(value.start) && Position.isPosition(value.end));
  }
  get isEmpty() { return this.start.isEqual(this.end); }
  get isSingleLine() { return this.start.line === this.end.line; }
  contains(other) {
    if (Range.isRange(other)) return this.contains(other.start) && this.contains(other.end);
    return this.start.isBeforeOrEqual(other) && this.end.isAfterOrEqual(other);
  }
  isEqual(other) {
    return this.start.isEqual(other.start) && this.end.isEqual(other.end);
  }
  intersection(other) {
    const start = this.start.isAfter(other.start) ? this.start : other.start;
    const end = this.end.isBefore(other.end) ? this.end : other.end;
    return start.isAfter(end) ? undefined : new Range(start, end);
  }
  union(other) {
    const start = this.start.isBefore(other.start) ? this.start : other.start;
    const end = this.end.isAfter(other.end) ? this.end : other.end;
    return new Range(start, end);
  }
  with(start = this.start, end = this.end) {
    if (start && !Position.isPosition(start)) {
      return this.with(start.start || this.start, start.end || this.end);
    }
    return new Range(start, end);
  }
  toJSON() {
    return { start: this.start.toJSON(), end: this.end.toJSON() };
  }
}

// A range with a direction: where it was begun, and where the cursor is.
class Selection extends Range {
  constructor(a, b, c, d) {
    const anchor = typeof a === 'number' ? new Position(a, b) : new Position(a.line, a.character);
    const active = typeof a === 'number' ? new Position(c, d) : new Position(b.line, b.character);
    super(anchor, active);
    this.anchor = anchor;
    this.active = active;
  }
  get isReversed() { return this.anchor.isAfter(this.active); }
}

class Location {
  constructor(uri, rangeOrPosition) {
    this.uri = uri;
    this.range = Range.isRange(rangeOrPosition)
      ? rangeOrPosition
      : new Range(rangeOrPosition, rangeOrPosition);
  }
}

class TextEdit {
  constructor(range, newText) {
    this.range = range;
    this.newText = newText;
  }
  static replace(range, newText) { return new TextEdit(range, newText); }
  static insert(position, newText) { return new TextEdit(new Range(position, position), newText); }
  static delete(range) { return new TextEdit(range, ''); }
  static setEndOfLine(eol) {
    const edit = new TextEdit(new Range(0, 0, 0, 0), '');
    edit.newEol = eol;
    return edit;
  }
}

// Changes to several files, made at once: texts by file, and files made,
// renamed or deleted on the way.
class WorkspaceEdit {
  constructor() {
    this._edits = new Map();
    this._files = [];
  }
  _of(uri) {
    const key = uri.toString();
    if (!this._edits.has(key)) this._edits.set(key, { uri, edits: [] });
    return this._edits.get(key).edits;
  }
  replace(uri, range, newText) { this._of(uri).push(new TextEdit(range, newText)); }
  insert(uri, position, newText) { this._of(uri).push(TextEdit.insert(position, newText)); }
  delete(uri, range) { this._of(uri).push(TextEdit.delete(range)); }
  has(uri) { return this._edits.has(uri.toString()); }
  set(uri, edits) {
    if (!edits) this._edits.delete(uri.toString());
    else this._edits.set(uri.toString(), { uri, edits: edits.filter((edit) => edit && edit.range) });
  }
  get(uri) {
    const entry = this._edits.get(uri.toString());
    return entry ? entry.edits : [];
  }
  entries() { return [...this._edits.values()].map(({ uri, edits }) => [uri, edits]); }
  get size() { return this._edits.size + this._files.length; }
  createFile(uri, options) { this._files.push({ kind: 'create', uri, options: options || {} }); }
  deleteFile(uri, options) { this._files.push({ kind: 'delete', uri, options: options || {} }); }
  renameFile(oldUri, newUri, options) {
    this._files.push({ kind: 'rename', uri: oldUri, newUri, options: options || {} });
  }
}

class CancellationTokenSource {
  constructor() {
    const emitter = new EventEmitter();
    this._emitter = emitter;
    this.token = { isCancellationRequested: false, onCancellationRequested: emitter.event };
  }
  cancel() {
    if (this.token.isCancellationRequested) return;
    this.token.isCancellationRequested = true;
    this._emitter.fire();
  }
  dispose() { this._emitter.dispose(); }
}

class CancellationError extends Error {
  constructor() {
    super('Canceled');
    this.name = 'Canceled';
  }
}

class MarkdownString {
  constructor(value = '', supportThemeIcons = false) {
    this.value = value;
    this.supportThemeIcons = supportThemeIcons;
    this.isTrusted = false;
    this.supportHtml = false;
  }
  appendText(text) {
    this.value += String(text).replace(/[\\`*_{}[\]()#+\-!~]/g, '\\$&');
    return this;
  }
  appendMarkdown(text) {
    this.value += text;
    return this;
  }
  appendCodeblock(code, language = '') {
    this.value += `\n\`\`\`${language}\n${code}\n\`\`\`\n`;
    return this;
  }
}

class ThemeColor {
  constructor(id) { this.id = id; }
}

class ThemeIcon {
  constructor(id, color) {
    this.id = id;
    this.color = color;
  }
}
ThemeIcon.File = new ThemeIcon('file');
ThemeIcon.Folder = new ThemeIcon('folder');

// A pattern for files under one folder.
class RelativePattern {
  constructor(base, pattern) {
    this.baseUri = typeof base === 'string' ? Uri.file(base) : base.uri || base;
    this.base = this.baseUri.fsPath;
    this.pattern = pattern;
  }
}

class FileSystemError extends Error {
  constructor(messageOrUri, code = 'Unknown') {
    super(Uri.isUri(messageOrUri) ? messageOrUri.toString() : messageOrUri || code);
    this.code = code;
    this.name = `${code} (FileSystemError)`;
  }
  static FileNotFound(at) { return new FileSystemError(at, 'FileNotFound'); }
  static FileExists(at) { return new FileSystemError(at, 'FileExists'); }
  static FileNotADirectory(at) { return new FileSystemError(at, 'FileNotADirectory'); }
  static FileIsADirectory(at) { return new FileSystemError(at, 'FileIsADirectory'); }
  static NoPermissions(at) { return new FileSystemError(at, 'NoPermissions'); }
  static Unavailable(at) { return new FileSystemError(at, 'Unavailable'); }
}

// A file pattern as VS Code writes them (`**/*.{ts,js}`), as an expression
// for a path with forward slashes.
function glob(pattern) {
  let out = '';
  let braces = 0;
  for (let i = 0; i < pattern.length; i++) {
    const c = pattern[i];
    if (c === '*') {
      if (pattern[i + 1] === '*') {
        // `**/` is any number of folders, none too.
        if (pattern[i + 2] === '/') {
          out += '(?:.*/)?';
          i += 2;
        } else {
          out += '.*';
          i += 1;
        }
      } else {
        out += '[^/]*';
      }
    } else if (c === '?') {
      out += '[^/]';
    } else if (c === '{') {
      out += '(?:';
      braces += 1;
    } else if (c === '}' && braces) {
      out += ')';
      braces -= 1;
    } else if (c === ',' && braces) {
      out += '|';
    } else if (c === '[') {
      const end = pattern.indexOf(']', i);
      if (end < 0) {
        out += '\\[';
      } else {
        out += '[' + pattern.slice(i + 1, end).replace(/^!/, '^').replace(/\\/g, '\\\\') + ']';
        i = end;
      }
    } else {
      out += c.replace(/[.+^$()|\\]/, '\\$&');
    }
  }
  return new RegExp(`^${out}$`);
}

const enums = {
  StatusBarAlignment: { Left: 1, Right: 2 },
  ConfigurationTarget: { Global: 1, Workspace: 2, WorkspaceFolder: 3 },
  ViewColumn: { Active: -1, Beside: -2, One: 1, Two: 2, Three: 3, Four: 4, Five: 5, Six: 6, Seven: 7, Eight: 8, Nine: 9 },
  EndOfLine: { LF: 1, CRLF: 2 },
  TextEditorRevealType: { Default: 0, InCenter: 1, InCenterIfOutsideViewport: 2, AtTop: 3 },
  TextEditorSelectionChangeKind: { Keyboard: 1, Mouse: 2, Command: 3 },
  TextDocumentChangeReason: { Undo: 1, Redo: 2 },
  TextDocumentSaveReason: { Manual: 1, AfterDelay: 2, FocusOut: 3 },
  FileType: { Unknown: 0, File: 1, Directory: 2, SymbolicLink: 64 },
  ProgressLocation: { SourceControl: 1, Window: 10, Notification: 15 },
  LogLevel: { Off: 0, Trace: 1, Debug: 2, Info: 3, Warning: 4, Error: 5 },
  ExtensionMode: { Production: 1, Development: 2, Test: 3 },
  ExtensionKind: { UI: 1, Workspace: 2 },
  UIKind: { Desktop: 1, Web: 2 },
  ColorThemeKind: { Light: 1, Dark: 2, HighContrast: 3, HighContrastLight: 4 },
  QuickPickItemKind: { Separator: -1, Default: 0 },
  QuickInputButtons: { Back: { iconPath: new ThemeIcon('arrow-left') } },
};

module.exports = {
  report,
  glob,
  enums,
  classes: {
    Disposable,
    EventEmitter,
    Uri,
    Position,
    Range,
    Selection,
    Location,
    TextEdit,
    WorkspaceEdit,
    CancellationTokenSource,
    CancellationError,
    MarkdownString,
    ThemeColor,
    ThemeIcon,
    RelativePattern,
    FileSystemError,
  },
};
