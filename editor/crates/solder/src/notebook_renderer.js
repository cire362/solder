// Only the extension's declared module is loaded. Output bytes are data,
// never inserted as HTML or as a script by this loader.
const config = __SOLDER_OUTPUT__;
const element = document.getElementById('output');
const api = acquireVsCodeApi();
const listeners = new Set();
window.addEventListener('message', event => listeners.forEach(listener => listener(event.data)));
const disposable = () => ({ dispose() {} });
const bytes = () => {
  const base64 = config.item.data || config.item.picture;
  return base64 ? Uint8Array.from(atob(base64), value => value.charCodeAt(0))
    : new TextEncoder().encode(config.item.text || '');
};
const text = () => new TextDecoder().decode(bytes());
const output = {
  id: config.id, mime: config.item.mime, metadata: config.item.metadata || {},
  data: bytes, text, json: () => JSON.parse(text()),
  blob: () => new Blob([bytes()], { type: config.item.mime }),
};
try {
  const module = await import(config.module);
  const renderer = await module.activate({
    getState: api.getState, setState: api.setState,
    postMessage: api.postMessage,
    onDidReceiveMessage(listener) {
      listeners.add(listener);
      return { dispose: () => listeners.delete(listener) };
    },
    getRenderer: async () => undefined,
    workspace: { isTrusted: false },
    settings: { outputLineLimit: 200, outputScrolling: true, outputWordWrap: true, linkifyFilePaths: false, minimalError: false },
    onDidChangeSettings: disposable,
  });
  await renderer.renderOutputItem(output, element,
    { isCancellationRequested: false, onCancellationRequested: disposable });
  window.addEventListener('beforeunload', () => renderer.disposeOutputItem?.(output.id), { once: true });
} catch (error) {
  element.textContent = `The renderer could not draw this output: ${error.message || error}`;
}
