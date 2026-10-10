'use strict';
// Each page belongs to this host. Native views own the browser; this side
// keeps the objects an extension listens to and receives messages from.
const { classes: { Uri, EventEmitter } } = require('./types');

module.exports = function buildWebviews(core) {
  const panels = new Map();
  const providers = new Map();
  // What opens files of a kind as a page in place of their text.
  const editors = new Map();
  const never = { isCancellationRequested: false, onCancellationRequested: () => ({ dispose() {} }) };
  let next = 0;
  // What a view is called: its manifest says, the code only knows its id.
  const named = (id) => Object.values((core.manifest.contributes && core.manifest.contributes.views) || {})
    .flat().find((view) => view.id === id)?.name || id;
  function origin(id, scheme) {
    return process.platform === 'win32' ? `http://${scheme}.${id}` : `${scheme}://${id}`;
  }
  function make(viewType, title, column, options = {}, id) {
    id = id || `page${++next}`;
    const received = new EventEmitter();
    const disposed = new EventEmitter();
    const changed = new EventEmitter();
    let html = '', visible = true, active = true, gone = false;
    let pageOptions = { ...options };
    let panelOptions = { ...options };
    const webview = {
      get html() { return html; },
      set html(value) { html = String(value || ''); publish(); },
      get options() { return pageOptions; },
      set options(value) { pageOptions = { ...value }; publish(); },
      get cspSource() { return origin(id, 'solder-resource'); },
      asWebviewUri(uri) {
        // Uri escapes each segment, including spaces, only once.
        return Uri.parse(origin(id, 'solder-resource')).with({ path: uri.fsPath });
      },
      onDidReceiveMessage: received.event,
      postMessage: (message) => gone ? Promise.resolve(false)
        : core.request('webview.post', { id, message: core.plain(message) }),
    };
    const panel = {
      viewType,
      get title() { return title; },
      set title(value) { title = String(value); publish(); },
      get viewColumn() { return column; },
      get active() { return active; },
      get visible() { return visible; },
      get options() { return panelOptions; },
      set options(value) { panelOptions = { ...value }; publish(); },
      webview,
      onDidDispose: disposed.event,
      onDidChangeViewState: changed.event,
      reveal(newColumn, preserveFocus) {
        if (gone) return;
        column = newColumn || column;
        core.notify('webview.reveal', { id, column, preserveFocus: !!preserveFocus });
      },
      dispose() {
        if (gone) return;
        gone = true;
        panels.delete(id);
        core.notify('webview', { id, gone: true });
        disposed.fire();
        received.dispose(); disposed.dispose(); changed.dispose();
      },
    };
    function publish() {
      if (gone) return;
      const roots = pageOptions.localResourceRoots === undefined
        ? [core.extensionDir, ...core.state.folders]
        : pageOptions.localResourceRoots.filter(Uri.isUri).map(u => u.fsPath);
      core.notify('webview', {
        id, viewType, title, column, html,
        options: { enableScripts: !!pageOptions.enableScripts, localResourceRoots: roots },
      });
    }
    const entry = { panel, received, change(state) {
      visible = !!state.visible; active = !!state.active;
      if (Number.isInteger(state.column)) column = state.column;
      changed.fire({ webviewPanel: panel });
    } };
    panels.set(id, entry);
    publish();
    return panel;
  }
  return {
    windowMembers: {
      createWebviewPanel(type, title, showOptions, options) {
        const column = typeof showOptions === 'number' ? showOptions : showOptions && showOptions.viewColumn;
        return make(type, title, column || 1, options);
      },
      registerWebviewViewProvider(id, provider) {
        providers.set(id, provider);
        core.notify('view', { id, title: named(id), kind: 'webview' });
        return { dispose() { if (providers.get(id) === provider) {
          providers.delete(id); core.notify('view', { id, gone: true });
        } } };
      },
      registerCustomEditorProvider(viewType, provider) {
        editors.set(viewType, provider);
        return { dispose() { if (editors.get(viewType) === provider) editors.delete(viewType); } };
      },
      registerWebviewPanelSerializer() {
        core.missing('window.registerWebviewPanelSerializer');
        return { dispose() {} };
      },
    },
    asked: {
      'webview.resolve': async ({ id }) => {
        const provider = providers.get(id);
        if (!provider) throw new Error(`No webview provider ${id}`);
        const known = panels.get(`view${id}`);
        if (known) { known.panel.reveal(); return true; }
        const panel = make(id, named(id), 1, {}, `view${id}`);
        const visible = new EventEmitter();
        panel.onDidChangeViewState(() => visible.fire());
        await provider.resolveWebviewView({
          viewType: id, webview: panel.webview,
          get title() { return panel.title; }, set title(t) { panel.title = t; },
          get visible() { return panel.visible; },
          onDidChangeVisibility: visible.event,
          onDidDispose: panel.onDidDispose,
          show: preserve => panel.reveal(undefined, preserve),
        }, { state: undefined }, never);
        return true;
      },
      // A file opened with an editor of the extension's: a page that is
      // given the file. One that edits text gets the document the editor
      // has, so what it changes and what is typed elsewhere stay one text;
      // a file that is not open is read as it is on disk.
      'customEditor.open': async ({ viewType, uri }) => {
        const provider = editors.get(viewType);
        if (!provider) throw new Error(`No editor ${viewType}`);
        const file = Uri.parse(uri);
        const panel = make(viewType, file.path.split('/').pop(), 1, {});
        if (provider.resolveCustomTextEditor) {
          await provider.resolveCustomTextEditor(core.docs.get(file) || core.docs.read(file), panel, never);
        } else {
          const document = await provider.openCustomDocument(file, { backupId: undefined, untitledDocumentData: undefined }, never);
          panel.onDidDispose(() => document && document.dispose && document.dispose());
          await provider.resolveCustomEditor(document, panel, never);
        }
        return true;
      },
    },
    told: {
      'webview.message': ({ id, message }) => panels.get(id)?.received.fire(message),
      'webview.state': ({ id, ...state }) => panels.get(id)?.change(state),
      'webview.closed': ({ id }) => panels.get(id)?.panel.dispose(),
    },
  };
};
