# Vue grammar for tests

A real extension language, so the tests load a grammar the way the editor
does: compiled to WebAssembly, with its queries in files.

- `vue.wasm`: tree-sitter-vue at 7e48557b, built by Zed's extension builder.
  MIT, https://github.com/tree-sitter-grammars/tree-sitter-vue
- `highlights.scm`, `injections.scm`: from Zed's Vue extension 0.4.0.
  Apache-2.0, https://github.com/zed-extensions/vue
