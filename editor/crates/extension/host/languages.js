'use strict';
// Language features an extension gives in code: completions, hovers,
// definitions and the rest of `vscode.languages`.
//
// The editor already knows how to ask a language server for all of them,
// so to the editor this host IS a language server: it is asked in the
// Language Server Protocol, and what it is asked is answered by the
// providers the extension registered. Nothing of the editor's own had to
// learn about extensions for a feature a server could already give.

const { classes, enums } = require('./types');

const { Disposable, EventEmitter, Uri, Position, Range, Location, CancellationTokenSource } = classes;

// ------------------------------------------------------------------ types

class SnippetString {
  constructor(value = '') {
    this.value = value;
    this._next = 1;
  }
  appendText(text) {
    this.value += String(text).replace(/[$}\\]/g, '\\$&');
    return this;
  }
  appendTabstop(number = this._next++) {
    this.value += `$${number}`;
    return this;
  }
  appendPlaceholder(value, number = this._next++) {
    const text = typeof value === 'function' ? (value(new SnippetString()) || {}).value || '' : String(value).replace(/[$}\\]/g, '\\$&');
    this.value += `\${${number}:${text}}`;
    return this;
  }
  appendChoice(values, number = this._next++) {
    this.value += `\${${number}|${values.map((value) => String(value).replace(/[,|\\]/g, '\\$&')).join(',')}|}`;
    return this;
  }
  appendVariable(name, fallback) {
    this.value += fallback === undefined ? `\${${name}}` : `\${${name}:${typeof fallback === 'string' ? fallback : ''}}`;
    return this;
  }
}

class CompletionItem {
  constructor(label, kind) {
    this.label = label;
    this.kind = kind;
  }
}

class CompletionList {
  constructor(items = [], isIncomplete = false) {
    this.items = items;
    this.isIncomplete = isIncomplete;
  }
}

class Hover {
  constructor(contents, range) {
    this.contents = Array.isArray(contents) ? contents : [contents];
    this.range = range;
  }
}

class Diagnostic {
  constructor(range, message, severity = enums.DiagnosticSeverity.Error) {
    this.range = range;
    this.message = message;
    this.severity = severity;
  }
}

class DiagnosticRelatedInformation {
  constructor(location, message) {
    this.location = location;
    this.message = message;
  }
}

class SymbolInformation {
  // (name, kind, containerName, location), or the older (name, kind,
  // range, uri, containerName).
  constructor(name, kind, a, b, c) {
    this.name = name;
    this.kind = kind;
    if (b instanceof Location || (b && b.uri && b.range)) {
      this.containerName = a;
      this.location = b;
    } else {
      this.location = new Location(b, a);
      this.containerName = c;
    }
  }
}

class DocumentSymbol {
  constructor(name, detail, kind, range, selectionRange) {
    this.name = name;
    this.detail = detail;
    this.kind = kind;
    this.range = range;
    this.selectionRange = selectionRange;
    this.children = [];
  }
}

// What a code action is for, as a path: `refactor.extract.function`.
class CodeActionKind {
  constructor(value) {
    this.value = value;
  }
  append(part) {
    return new CodeActionKind(this.value ? `${this.value}.${part}` : part);
  }
  contains(other) {
    return this.value === other.value || other.value.startsWith(`${this.value}.`) || this.value === '';
  }
  intersects(other) {
    return this.contains(other) || other.contains(this);
  }
}
CodeActionKind.Empty = new CodeActionKind('');
CodeActionKind.QuickFix = new CodeActionKind('quickfix');
CodeActionKind.Refactor = new CodeActionKind('refactor');
CodeActionKind.RefactorExtract = new CodeActionKind('refactor.extract');
CodeActionKind.RefactorInline = new CodeActionKind('refactor.inline');
CodeActionKind.RefactorMove = new CodeActionKind('refactor.move');
CodeActionKind.RefactorRewrite = new CodeActionKind('refactor.rewrite');
CodeActionKind.Source = new CodeActionKind('source');
CodeActionKind.SourceOrganizeImports = new CodeActionKind('source.organizeImports');
CodeActionKind.SourceFixAll = new CodeActionKind('source.fixAll');
CodeActionKind.Notebook = new CodeActionKind('notebook');

class CodeAction {
  constructor(title, kind) {
    this.title = title;
    this.kind = kind;
  }
}

class CodeLens {
  constructor(range, command) {
    this.range = range;
    this.command = command;
  }
  get isResolved() {
    return !!this.command;
  }
}

class ParameterInformation {
  constructor(label, documentation) {
    this.label = label;
    this.documentation = documentation;
  }
}

class SignatureInformation {
  constructor(label, documentation) {
    this.label = label;
    this.documentation = documentation;
    this.parameters = [];
  }
}

class SignatureHelp {
  constructor() {
    this.signatures = [];
    this.activeSignature = 0;
    this.activeParameter = 0;
  }
}

class InlayHint {
  constructor(position, label, kind) {
    this.position = position;
    this.label = label;
    this.kind = kind;
  }
}
class InlayHintLabelPart {
  constructor(value) {
    this.value = value;
  }
}

// Which kinds of words a provider tells apart, and what else it says of
// one: its answers name them by their place in these lists.
class SemanticTokensLegend {
  constructor(tokenTypes, tokenModifiers = []) {
    this.tokenTypes = tokenTypes;
    this.tokenModifiers = tokenModifiers;
  }
}
class SemanticTokens {
  constructor(data, resultId) {
    this.data = data;
    this.resultId = resultId;
  }
}
class SemanticTokensEdit {
  constructor(start, deleteCount, data) {
    Object.assign(this, { start, deleteCount, data });
  }
}
class SemanticTokensEdits {
  constructor(edits, resultId) {
    this.edits = edits;
    this.resultId = resultId;
  }
}
// Gathers words in any order and gives them as the run of numbers the
// answer is: each by how far it is from the one before.
class SemanticTokensBuilder {
  constructor(legend) {
    this._legend = legend;
    this._tokens = [];
  }
  push(a, b, c, d, e) {
    if (typeof a === 'number') {
      this._tokens.push([a, b, c, d, e || 0]);
      return;
    }
    if (!this._legend) throw new Error('A builder that is given names needs a legend');
    const type = this._legend.tokenTypes.indexOf(b);
    if (type < 0) throw new Error(`'${b}' is not in the legend`);
    let modifiers = 0;
    for (const name of c || []) {
      const bit = this._legend.tokenModifiers.indexOf(name);
      if (bit >= 0) modifiers |= 1 << bit;
    }
    this._tokens.push([a.start.line, a.start.character, a.end.character - a.start.character, type, modifiers]);
  }
  build(resultId) {
    const sorted = [...this._tokens].sort((x, y) => x[0] - y[0] || x[1] - y[1]);
    const data = [];
    let [line, start] = [0, 0];
    for (const [tokenLine, tokenStart, length, type, modifiers] of sorted) {
      data.push(tokenLine - line, tokenLine === line ? tokenStart - start : tokenStart, length, type, modifiers);
      [line, start] = [tokenLine, tokenStart];
    }
    return new SemanticTokens(new Uint32Array(data), resultId);
  }
}

// Data of features the editor has no place for yet: an extension builds
// them all the same, and its own classes are built on some of them.
class DocumentHighlight {
  constructor(range, kind = 0) {
    this.range = range;
    this.kind = kind;
  }
}
class DocumentLink {
  constructor(range, target) {
    this.range = range;
    this.target = target;
  }
}
class FoldingRange {
  constructor(start, end, kind) {
    this.start = start;
    this.end = end;
    this.kind = kind;
  }
}
class SelectionRange {
  constructor(range, parent) {
    this.range = range;
    this.parent = parent;
  }
}
class Color {
  constructor(red, green, blue, alpha) {
    Object.assign(this, { red, green, blue, alpha });
  }
}
class ColorInformation {
  constructor(range, color) {
    this.range = range;
    this.color = color;
  }
}
class ColorPresentation {
  constructor(label) {
    this.label = label;
  }
}
class CallHierarchyItem {
  constructor(kind, name, detail, uri, range, selectionRange) {
    Object.assign(this, { kind, name, detail, uri, range, selectionRange });
  }
}
class TypeHierarchyItem {
  constructor(kind, name, detail, uri, range, selectionRange) {
    Object.assign(this, { kind, name, detail, uri, range, selectionRange });
  }
}

const languageEnums = {
  CompletionItemKind: {
    Text: 0, Method: 1, Function: 2, Constructor: 3, Field: 4, Variable: 5, Class: 6, Interface: 7,
    Module: 8, Property: 9, Unit: 10, Value: 11, Enum: 12, Keyword: 13, Snippet: 14, Color: 15,
    File: 16, Reference: 17, Folder: 18, EnumMember: 19, Constant: 20, Struct: 21, Event: 22,
    Operator: 23, TypeParameter: 24, User: 25, Issue: 26,
  },
  CompletionItemTag: { Deprecated: 1 },
  CompletionTriggerKind: { Invoke: 0, TriggerCharacter: 1, TriggerForIncompleteCompletions: 2 },
  DiagnosticSeverity: { Error: 0, Warning: 1, Information: 2, Hint: 3 },
  DiagnosticTag: { Unnecessary: 1, Deprecated: 2 },
  SymbolKind: {
    File: 0, Module: 1, Namespace: 2, Package: 3, Class: 4, Method: 5, Property: 6, Field: 7,
    Constructor: 8, Enum: 9, Interface: 10, Function: 11, Variable: 12, Constant: 13, String: 14,
    Number: 15, Boolean: 16, Array: 17, Object: 18, Key: 19, Null: 20, EnumMember: 21, Struct: 22,
    Event: 23, Operator: 24, TypeParameter: 25,
  },
  SymbolTag: { Deprecated: 1 },
  CodeActionTriggerKind: { Invoke: 1, Automatic: 2 },
  SignatureHelpTriggerKind: { Invoke: 1, TriggerCharacter: 2, ContentChange: 3 },
  DocumentHighlightKind: { Text: 0, Read: 1, Write: 2 },
  FoldingRangeKind: { Comment: 1, Imports: 2, Region: 3 },
  InlayHintKind: { Type: 1, Parameter: 2 },
  IndentAction: { None: 0, Indent: 1, IndentOutdent: 2, Outdent: 3 },
  LanguageStatusSeverity: { Information: 0, Warning: 1, Error: 2 },
};
Object.assign(enums, languageEnums);

const languageClasses = {
  SnippetString, CompletionItem, CompletionList, Hover, Diagnostic, DiagnosticRelatedInformation,
  SymbolInformation, DocumentSymbol, CodeActionKind, CodeAction, CodeLens, ParameterInformation,
  SignatureInformation, SignatureHelp, InlayHint, InlayHintLabelPart, SemanticTokensLegend,
  SemanticTokens, SemanticTokensEdit, SemanticTokensEdits, SemanticTokensBuilder,
  DocumentHighlight, DocumentLink, FoldingRange, SelectionRange,
  Color, ColorInformation, ColorPresentation, CallHierarchyItem, TypeHierarchyItem,
};

// --------------------------------------------------------- to the protocol

function markdown(value) {
  if (value === undefined || value === null) return undefined;
  if (typeof value === 'string') return value;
  if (typeof value.value === 'string') {
    // `{ language, value }` is code in that language.
    return value.language ? `\`\`\`${value.language}\n${value.value}\n\`\`\`` : value.value;
  }
  return String(value);
}
function documentation(value) {
  const text = markdown(value);
  return text === undefined ? undefined : { kind: 'markdown', value: text };
}
function range(value) {
  return value ? { start: position(value.start), end: position(value.end) } : undefined;
}
function position(value) {
  return { line: value.line, character: value.character };
}
function location(value) {
  if (!value) return undefined;
  // A link names what it leads to and the part of it to show.
  if (value.targetUri) {
    return { uri: value.targetUri.toString(), range: range(value.targetSelectionRange || value.targetRange) };
  }
  return { uri: value.uri.toString(), range: range(value.range) };
}
function locations(value) {
  return [].concat(value || []).map(location).filter(Boolean);
}
function textEdits(edits) {
  return (edits || []).filter((edit) => edit && edit.range).map((edit) => ({ range: range(edit.range), newText: edit.newText }));
}
function workspaceEdit(edit) {
  if (!edit) return undefined;
  const changes = {};
  for (const [uri, edits] of edit.entries()) changes[uri.toString()] = textEdits(edits);
  return { changes };
}
function diagnostic(value) {
  const code = value.code && typeof value.code === 'object' ? value.code.value : value.code;
  return {
    range: range(value.range),
    message: String(value.message),
    severity: (value.severity === undefined ? 0 : value.severity) + 1,
    source: value.source,
    code,
    tags: value.tags,
  };
}
function symbol(value) {
  if (value.location) {
    return { name: value.name, kind: value.kind + 1, location: location(value.location), containerName: value.containerName };
  }
  return {
    name: value.name || '?',
    detail: value.detail,
    kind: value.kind + 1,
    range: range(value.range),
    selectionRange: range(value.selectionRange || value.range),
    children: (value.children || []).map(symbol),
    tags: value.tags,
  };
}

// --------------------------------------------------------------- the rest

module.exports = function build(core) {
  const { docs, notify } = core;
  // What the extension registered: its kind, which documents it is for,
  // and the provider.
  const providers = [];
  const collections = new Set();
  const diagnosticsChanged = new EventEmitter();
  const never = new CancellationTokenSource().token;

  // How well a selector fits a document: 0 is not at all.
  function match(selector, document) {
    if (Array.isArray(selector)) return Math.max(0, ...selector.map((one) => match(one, document)));
    if (typeof selector === 'string') return selector === '*' ? 5 : selector === document.languageId ? 10 : 0;
    if (!selector) return 0;
    let score = 5;
    if (selector.scheme) {
      if (selector.scheme !== document.uri.scheme && selector.scheme !== '*') return 0;
      score = 10;
    }
    if (selector.language) {
      if (selector.language !== document.languageId && selector.language !== '*') return 0;
      score = selector.language === '*' ? score : 10;
    }
    if (selector.pattern) {
      const pattern = typeof selector.pattern === 'string' ? selector.pattern : selector.pattern.pattern;
      const base = typeof selector.pattern === 'string' ? '' : selector.pattern.base;
      const file = base && document.uri.fsPath.startsWith(base + '/') ? document.uri.fsPath.slice(base.length + 1) : document.uri.fsPath;
      const { glob } = require('./types');
      if (!glob(pattern).test(file) && !glob(`**/${pattern}`).test(file)) return 0;
      score = 10;
    }
    return score;
  }
  // The languages a selector names, `*` for one that names none.
  function languagesOf(selector) {
    if (Array.isArray(selector)) return selector.flatMap(languagesOf);
    if (typeof selector === 'string') return [selector];
    return [selector && selector.language ? selector.language : '*'];
  }

  // The editor is told which languages this extension has something for,
  // once for all that was registered in one go.
  let telling = false;
  function changed() {
    if (telling) return;
    telling = true;
    setTimeout(() => {
      telling = false;
      const languages = new Set(providers.flatMap((entry) => languagesOf(entry.selector)));
      // What it reports is about whatever file it reports on.
      if (collections.size) languages.add('*');
      notify('providers', { languages: [...languages] });
    }, 20);
  }
  function register(kind, selector, provider, more) {
    const entry = { kind, selector, provider, ...more };
    providers.push(entry);
    changed();
    return new Disposable(() => {
      const at = providers.indexOf(entry);
      if (at >= 0) providers.splice(at, 1);
      changed();
    });
  }
  function of(kind, document) {
    return providers
      .filter((entry) => entry.kind === kind && (!document || match(entry.selector, document) > 0))
      .sort((a, b) => (document ? match(b.selector, document) - match(a.selector, document) : 0));
  }
  // The answer of the first provider that has one.
  async function first(kind, document, ask) {
    for (const entry of of(kind, document)) {
      try {
        const answer = await ask(entry.provider, entry);
        if (answer !== undefined && answer !== null && !(Array.isArray(answer) && !answer.length)) return answer;
      } catch (error) {
        core.say('error', [error && error.stack || error]);
      }
    }
    return undefined;
  }
  // The answers of every provider, in one list.
  async function every(kind, document, ask) {
    const all = [];
    for (const entry of of(kind, document)) {
      try {
        const answer = await ask(entry.provider, entry);
        if (answer) all.push(...[].concat(answer).map((one) => ({ one, entry })));
      } catch (error) {
        core.say('error', [error && error.stack || error]);
      }
    }
    return all;
  }

  // A command the extension hands out with an answer. What it is given
  // may be anything, so it stays here and the editor gets its number.
  const handed = [];
  function command(value) {
    if (!value || !value.command) return undefined;
    handed.push(value);
    if (handed.length > 2000) handed.splice(0, 1000);
    return { title: String(value.title || value.command), command: 'solder.run', arguments: [handed.indexOf(value)] };
  }

  // ------------------------------------------------------------ diagnostics

  function publish(uri) {
    const key = uri.toString();
    const all = [];
    for (const collection of collections) all.push(...(collection._items.get(key) || { list: [] }).list);
    notify('lsp', { message: { jsonrpc: '2.0', method: 'textDocument/publishDiagnostics', params: { uri: key, diagnostics: all.map(diagnostic) } } });
  }
  function createDiagnosticCollection(name) {
    const collection = {
      name,
      _items: new Map(),
      set(target, list) {
        const entries = Array.isArray(target) ? target : [[target, list]];
        const touched = new Map();
        for (const [uri, items] of entries) {
          const key = uri.toString();
          // The same file given twice adds up; given nothing, it is clear.
          const before = touched.has(key) ? collection._items.get(key).list : [];
          if (items === undefined) collection._items.delete(key);
          else collection._items.set(key, { uri, list: before.concat(items) });
          touched.set(key, uri);
        }
        for (const uri of touched.values()) publish(uri);
        diagnosticsChanged.fire({ uris: [...touched.values()] });
      },
      delete(uri) {
        collection._items.delete(uri.toString());
        publish(uri);
        diagnosticsChanged.fire({ uris: [uri] });
      },
      clear() {
        const uris = [...collection._items.values()].map((entry) => entry.uri);
        collection._items.clear();
        uris.forEach(publish);
        if (uris.length) diagnosticsChanged.fire({ uris });
      },
      forEach(callback, thisArg) {
        for (const { uri, list } of collection._items.values()) callback.call(thisArg, uri, list, collection);
      },
      get: (uri) => (collection._items.get(uri.toString()) || { list: undefined }).list,
      has: (uri) => collection._items.has(uri.toString()),
      dispose() {
        collection.clear();
        collections.delete(collection);
        changed();
      },
      [Symbol.iterator]() {
        return [...collection._items.values()].map(({ uri, list }) => [uri, list])[Symbol.iterator]();
      },
    };
    collections.add(collection);
    changed();
    return collection;
  }
  function getDiagnostics(uri) {
    if (uri) return [...collections].flatMap((collection) => collection.get(uri) || []);
    const byFile = new Map();
    for (const collection of collections) {
      collection.forEach((file, list) => {
        const entry = byFile.get(file.toString()) || [file, []];
        entry[1].push(...list);
        byFile.set(file.toString(), entry);
      });
    }
    return [...byFile.values()];
  }
  // Everything it reported, said again: to an editor that just began to
  // listen, or about a file it just opened.
  function republish(only) {
    const files = new Map();
    for (const collection of collections) {
      for (const { uri } of collection._items.values()) files.set(uri.toString(), uri);
    }
    for (const [key, uri] of files) if (!only || only === key) publish(uri);
  }

  // ---------------------------------------------------------- the namespace

  const languages = core.namespace('languages', {
    match,
    getLanguages: async () => [...new Set(docs.all.map((document) => document.languageId).concat(['plaintext']))],
    createDiagnosticCollection,
    getDiagnostics,
    onDidChangeDiagnostics: diagnosticsChanged.event,
    registerCompletionItemProvider: (selector, provider, ...triggers) => register('completion', selector, provider, { triggers }),
    registerHoverProvider: (selector, provider) => register('hover', selector, provider),
    registerDefinitionProvider: (selector, provider) => register('definition', selector, provider),
    registerTypeDefinitionProvider: (selector, provider) => register('typeDefinition', selector, provider),
    registerImplementationProvider: (selector, provider) => register('implementation', selector, provider),
    registerDeclarationProvider: (selector, provider) => register('declaration', selector, provider),
    registerReferenceProvider: (selector, provider) => register('references', selector, provider),
    registerRenameProvider: (selector, provider) => register('rename', selector, provider),
    registerDocumentFormattingEditProvider: (selector, provider) => register('format', selector, provider),
    registerDocumentRangeFormattingEditProvider: (selector, provider) => register('rangeFormat', selector, provider),
    registerCodeActionsProvider: (selector, provider, metadata) => register('codeAction', selector, provider, { metadata }),
    registerCodeLensProvider: (selector, provider) => register('codeLens', selector, provider),
    registerDocumentSymbolProvider: (selector, provider) => register('documentSymbol', selector, provider),
    registerWorkspaceSymbolProvider: (provider) => register('workspaceSymbol', '*', provider),
    registerInlayHintsProvider: (selector, provider) => register('inlay', selector, provider),
    registerDocumentSemanticTokensProvider: (selector, provider, legend) => register('semantic', selector, provider, { legend }),
    registerDocumentRangeSemanticTokensProvider: (selector, provider, legend) => register('semanticRange', selector, provider, { legend }),
    registerSignatureHelpProvider(selector, provider, ...rest) {
      const metadata = rest.length === 1 && rest[0] && typeof rest[0] === 'object' ? rest[0] : undefined;
      const triggers = metadata ? metadata.triggerCharacters || [] : rest;
      return register('signature', selector, provider, { triggers });
    },
  });

  // ------------------------------------------------- what the editor asks

  // The provider that says what each word is. What its numbers mean is
  // said to the editor once, so there is one: the first registered.
  function coloring() {
    return providers.find((entry) => entry.kind === 'semantic') || providers.find((entry) => entry.kind === 'semanticRange');
  }

  function capabilities() {
    const has = (kind) => providers.some((entry) => entry.kind === kind);
    const colors = coloring();
    const triggers = (kind) => [...new Set(providers.filter((entry) => entry.kind === kind).flatMap((entry) => entry.triggers || []))];
    return {
      // The documents are known here already: the editor says every
      // change to the host itself, in order with what it asks.
      textDocumentSync: { openClose: true, change: 0 },
      completionProvider: has('completion') ? { triggerCharacters: triggers('completion') } : undefined,
      hoverProvider: has('hover') || undefined,
      definitionProvider: has('definition') || undefined,
      referencesProvider: has('references') || undefined,
      renameProvider: has('rename') ? { prepareProvider: providers.some((entry) => entry.kind === 'rename' && entry.provider.prepareRename) } : undefined,
      documentFormattingProvider: has('format') || has('rangeFormat') || undefined,
      // A code lens is offered where the editor offers what can be done
      // with a line: among its code actions.
      codeActionProvider: has('codeAction') || has('codeLens') ? { resolveProvider: true } : undefined,
      documentSymbolProvider: has('documentSymbol') || undefined,
      workspaceSymbolProvider: has('workspaceSymbol') || undefined,
      signatureHelpProvider: has('signature') ? { triggerCharacters: triggers('signature') } : undefined,
      inlayHintProvider: has('inlay') || undefined,
      semanticTokensProvider: colors
        ? { legend: { tokenTypes: colors.legend.tokenTypes, tokenModifiers: colors.legend.tokenModifiers || [] }, full: true }
        : undefined,
      executeCommandProvider: { commands: ['solder.run'] },
    };
  }

  function completionItem(item, entry) {
    const label = typeof item.label === 'string' ? item.label : item.label.label;
    const snippet = item.insertText instanceof SnippetString || (item.insertText && typeof item.insertText === 'object');
    const text = item.insertText === undefined ? undefined : snippet ? item.insertText.value : item.insertText;
    const where = item.range && (item.range.replacing || item.range.inserting || item.range);
    const out = {
      label,
      labelDetails: typeof item.label === 'string' ? undefined : { detail: item.label.detail, description: item.label.description },
      kind: item.kind === undefined ? undefined : Math.min(item.kind + 1, 25),
      detail: item.detail,
      documentation: documentation(item.documentation),
      sortText: item.sortText,
      filterText: item.filterText,
      preselect: item.preselect,
      insertTextFormat: snippet ? 2 : 1,
      commitCharacters: item.commitCharacters,
      additionalTextEdits: item.additionalTextEdits ? textEdits(item.additionalTextEdits) : undefined,
      command: command(item.command),
      tags: item.tags,
    };
    if (where) out.textEdit = { range: range(where), newText: text === undefined ? label : text };
    else out.insertText = text;
    void entry;
    return out;
  }

  // The actions last given, so that one chosen can be finished.
  let offered = [];
  function codeAction(action, entry) {
    // A bare command is an action that only runs it.
    if (action.command && typeof action.command === 'string') {
      return { title: action.title, command: command(action) };
    }
    offered.push({ action, entry });
    return {
      title: action.title,
      kind: action.kind ? action.kind.value : undefined,
      diagnostics: action.diagnostics ? action.diagnostics.map(diagnostic) : undefined,
      isPreferred: action.isPreferred,
      disabled: action.disabled,
      edit: workspaceEdit(action.edit),
      command: command(action.command),
      data: offered.length - 1,
    };
  }

  function signature(help) {
    return {
      signatures: (help.signatures || []).map((one) => ({
        label: one.label,
        documentation: documentation(one.documentation),
        parameters: (one.parameters || []).map((parameter) => ({ label: parameter.label, documentation: documentation(parameter.documentation) })),
        activeParameter: one.activeParameter,
      })),
      activeSignature: help.activeSignature || 0,
      activeParameter: help.activeParameter || 0,
    };
  }

  const asked = {
    initialize: async () => ({ capabilities: capabilities(), serverInfo: { name: core.extensionId } }),
    shutdown: async () => null,
    'textDocument/completion': async ({ document, at, params }) => {
      const kind = params.context ? params.context.triggerKind - 1 : 0;
      const context = { triggerKind: kind, triggerCharacter: params.context && params.context.triggerCharacter };
      let incomplete = false;
      const found = await every('completion', document, async (provider) => {
        const answer = await provider.provideCompletionItems(document, at, never, context);
        if (answer && !Array.isArray(answer)) {
          incomplete = incomplete || !!answer.isIncomplete;
          return answer.items;
        }
        return answer;
      });
      return { isIncomplete: incomplete, items: found.map(({ one, entry }) => completionItem(one, entry)) };
    },
    'textDocument/hover': async ({ document, at }) => {
      const hover = await first('hover', document, (provider) => provider.provideHover(document, at, never));
      if (!hover) return null;
      const text = [].concat(hover.contents).map(markdown).filter(Boolean).join('\n\n');
      return { contents: { kind: 'markdown', value: text }, range: range(hover.range) };
    },
    'textDocument/definition': async ({ document, at }) =>
      locations(await first('definition', document, (provider) => provider.provideDefinition(document, at, never))),
    'textDocument/references': async ({ document, at, params }) => {
      const context = { includeDeclaration: !!(params.context && params.context.includeDeclaration) };
      const found = await every('references', document, (provider) => provider.provideReferences(document, at, context, never));
      return found.map(({ one }) => location(one));
    },
    'textDocument/prepareRename': async ({ document, at }) => {
      const found = await first('rename', document, (provider) => (provider.prepareRename ? provider.prepareRename(document, at, never) : undefined));
      if (!found) return null;
      return found.range ? { range: range(found.range), placeholder: found.placeholder } : range(found);
    },
    'textDocument/rename': async ({ document, at, params }) =>
      workspaceEdit(await first('rename', document, (provider) => provider.provideRenameEdits(document, at, params.newName, never))) || null,
    'textDocument/formatting': async ({ document, params }) => {
      const options = { tabSize: params.options.tabSize, insertSpaces: params.options.insertSpaces };
      const whole = await first('format', document, (provider) => provider.provideDocumentFormattingEdits(document, options, never));
      if (whole) return textEdits(whole);
      // One that formats a part formats the whole as a part.
      const all = new Range(0, 0, document.lineCount, 0);
      return textEdits(await first('rangeFormat', document, (provider) => provider.provideDocumentRangeFormattingEdits(document, document.validateRange(all), options, never)));
    },
    'textDocument/codeAction': async ({ document, params }) => {
      const where = new Range(params.range.start.line, params.range.start.character, params.range.end.line, params.range.end.character);
      const only = params.context && params.context.only && params.context.only[0];
      const context = {
        diagnostics: getDiagnostics(document.uri).filter((one) => one.range.intersection(where)),
        only: only ? new CodeActionKind(only) : undefined,
        triggerKind: (params.context && params.context.triggerKind) || 1,
      };
      offered = [];
      const actions = await every('codeAction', document, (provider) => provider.provideCodeActions(document, where, context, never));
      const out = actions.map(({ one, entry }) => codeAction(one, entry));
      // The lenses of these lines, each as something to do.
      const lenses = await every('codeLens', document, (provider) => provider.provideCodeLenses(document, never));
      for (const { one, entry } of lenses) {
        if (one.range.start.line > where.end.line || one.range.end.line < where.start.line) continue;
        const lens = one.command || !entry.provider.resolveCodeLens ? one : (await entry.provider.resolveCodeLens(one, never)) || one;
        if (lens.command) out.push({ title: lens.command.title, command: command(lens.command) });
      }
      return out;
    },
    'codeAction/resolve': async ({ params }) => {
      const known = offered[params.data];
      if (!known || !known.entry.provider.resolveCodeAction) return params;
      const resolved = (await known.entry.provider.resolveCodeAction(known.action, never)) || known.action;
      return { ...params, edit: workspaceEdit(resolved.edit), command: command(resolved.command) };
    },
    'textDocument/documentSymbol': async ({ document }) => {
      const found = await every('documentSymbol', document, (provider) => provider.provideDocumentSymbols(document, never));
      return found.map(({ one }) => symbol(one));
    },
    'workspace/symbol': async ({ params }) => {
      const found = await every('workspaceSymbol', undefined, (provider) => provider.provideWorkspaceSymbols(params.query, never));
      return found.filter(({ one }) => one.location && one.location.range).map(({ one }) => symbol(one));
    },
    'textDocument/signatureHelp': async ({ document, at, params }) => {
      const context = { triggerKind: (params.context && params.context.triggerKind) || 1, triggerCharacter: params.context && params.context.triggerCharacter, isRetrigger: false };
      const help = await first('signature', document, (provider) => provider.provideSignatureHelp(document, at, never, context));
      return help ? signature(help) : null;
    },
    'textDocument/inlayHint': async ({ document, params }) => {
      const where = new Range(params.range.start.line, params.range.start.character, params.range.end.line, params.range.end.character);
      const found = await every('inlay', document, (provider) => provider.provideInlayHints(document, where, never));
      return found.map(({ one }) => ({
        position: position(one.position),
        label: typeof one.label === 'string' ? one.label : one.label.map((part) => ({ value: String(part.value) })),
        kind: one.kind,
        paddingLeft: one.paddingLeft,
        paddingRight: one.paddingRight,
      }));
    },
    'textDocument/semanticTokens/full': async ({ document }) => {
      const colors = coloring();
      if (!colors || !match(colors.selector, document)) return null;
      const all = document.validateRange(new Range(0, 0, document.lineCount, 0));
      const tokens = colors.kind === 'semantic'
        ? await colors.provider.provideDocumentSemanticTokens(document, never)
        : await colors.provider.provideDocumentRangeSemanticTokens(document, all, never);
      return tokens && tokens.data ? { data: Array.from(tokens.data) } : null;
    },
    'workspace/executeCommand': async ({ params }) => {
      const known = params.command === 'solder.run' ? handed[params.arguments[0]] : undefined;
      if (!known) throw new Error(`No command ${params.command}`);
      return core.plain(await core.vscode.commands.executeCommand(known.command, ...(known.arguments || [])));
    },
  };

  // One message of the protocol from the editor.
  async function lsp({ message }) {
    const params = message.params || {};
    const reply = (body) => notify('lsp', { message: { jsonrpc: '2.0', id: message.id, ...body } });
    if (message.id === undefined) {
      if (message.method === 'initialized') republish();
      if (message.method === 'textDocument/didOpen') republish(Uri.parse(params.textDocument.uri).toString());
      return;
    }
    const handler = asked[message.method];
    if (!handler) {
      reply({ error: { code: -32601, message: `No ${message.method} here` } });
      return;
    }
    try {
      const uri = params.textDocument && params.textDocument.uri;
      const document = uri ? docs.get(Uri.parse(uri)) : undefined;
      // A file the host was not told of has nothing to be asked about.
      if (uri && !document) {
        reply({ result: null });
        return;
      }
      const at = params.position ? new Position(params.position.line, params.position.character) : undefined;
      const result = await handler({ document, at, params });
      reply({ result: result === undefined ? null : result });
    } catch (error) {
      reply({ error: { code: -32603, message: error && error.message ? error.message : String(error) } });
    }
  }

  return { languages, lsp, classes: languageClasses, register, asked, capabilities, every, first, range, position };
};
