//! Grammars compiled to WebAssembly, the form extensions ship them in.
//!
//! A grammar's module is compiled once, when its language is first used. It
//! then runs inside a store that belongs to a parser, with no access to
//! anything but the text the parser hands it.

use std::sync::{Mutex, OnceLock};

use tree_sitter::{Parser, WasmStore, wasmtime::Engine};

use crate::LanguageSpec;

/// Parsers kept for reuse. A few are enough: one for the UI thread and one
/// for each background parse that overlaps it.
const POOL: usize = 4;

fn engine() -> &'static Engine {
    static ENGINE: OnceLock<Engine> = OnceLock::new();
    ENGINE.get_or_init(Engine::default)
}

fn pool() -> &'static Mutex<Vec<Parser>> {
    static PARSERS: Mutex<Vec<Parser>> = Mutex::new(Vec::new());
    &PARSERS
}

/// Compiles the grammar of `spec`. Slow: keep it off the UI thread.
pub fn load(spec: &LanguageSpec) -> Result<tree_sitter::Language, String> {
    let bytes =
        std::fs::read(&spec.grammar).map_err(|e| format!("{}: {e}", spec.grammar.display()))?;
    // The compiled module travels with the language; each parser's own store
    // instantiates it, so this store is needed only for the compilation.
    let mut store = WasmStore::new(engine()).map_err(|e| e.to_string())?;
    store
        .load_language(&spec.symbol, &bytes)
        .map_err(|e| e.to_string())
}

/// A parser that can run WebAssembly grammars.
pub fn parser() -> Option<Parser> {
    if let Some(parser) = pool().lock().unwrap().pop() {
        return Some(parser);
    }
    let mut parser = Parser::new();
    parser.set_wasm_store(WasmStore::new(engine()).ok()?).ok()?;
    Some(parser)
}

pub fn give_back(mut parser: Parser) {
    // The next user may be another language with other ranges.
    parser.reset();
    let _ = parser.set_included_ranges(&[]);
    let mut parsers = pool().lock().unwrap();
    if parsers.len() < POOL {
        parsers.push(parser);
    }
}
