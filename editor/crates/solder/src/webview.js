// Injected into the main frame before extension scripts. The browser's
// native bridge stays private; extensions see VS Code's three methods.
(() => {
  if (window !== window.top) return;
  const send = window.ipc.postMessage.bind(window.ipc);
  delete window.ipc;
  let acquired = false, state, initialTheme;
  window.acquireVsCodeApi = () => {
    if (acquired) throw new Error('The VS Code API was already acquired');
    acquired = true;
    return Object.freeze({
      postMessage: value => send(JSON.stringify({kind:'message', value})),
      getState: () => { loadConfig(); return state; },
      setState: value => { state = value; send(JSON.stringify({kind:'state', value})); return value; },
    });
  };
  window.__solderTheme = theme => {
    if (!document.body) return;
    document.body.classList.toggle('vscode-dark', theme.dark);
    document.body.classList.toggle('vscode-light', !theme.dark);
    for (const [name, value] of Object.entries(theme.colors)) document.documentElement.style.setProperty('--vscode-' + name, value);
    document.documentElement.style.setProperty('--vscode-font-family', theme.font);
    document.documentElement.style.setProperty('--vscode-font-size', theme.size + 'px');
    document.documentElement.style.setProperty('--vscode-editor-font-family', theme.code);
    document.documentElement.style.setProperty('--vscode-editor-font-size', theme.codeSize + 'px');
  };
  // Comment data is readable before a script in the body asks for state.
  const loadConfig = () => {
    const first = document.firstChild;
    if (first && first.nodeType === Node.COMMENT_NODE && first.data.startsWith('solder-config:')) {
      const config = JSON.parse(first.data.slice('solder-config:'.length));
      state = config.state;
      initialTheme = config.theme;
      window.__solderTheme(config.theme);
      first.remove();
      return config;
    }
  };
  const observer = new MutationObserver(() => { const config = loadConfig(); if (config) observer.disconnect(); });
  observer.observe(document, {childList:true});
  document.addEventListener('DOMContentLoaded', () => {
    loadConfig();
    if (initialTheme) window.__solderTheme(initialTheme);
    send(JSON.stringify({kind:'ready'}));
  }, {once:true});
  document.addEventListener('keydown', e => {
    if ((e.metaKey || e.ctrlKey) && ['w','p'].includes(e.key.toLowerCase())) {
      e.preventDefault(); send(JSON.stringify({kind:'key', key:e.key.toLowerCase() === 'w' ? 'close' : e.shiftKey ? 'commands' : 'files'}));
    }
  }, true);
})();
