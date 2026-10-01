//! Reads what Solder needs from a GGUF file's header: its parameter count,
//! how many of them each token uses (mixture-of-experts models use a few
//! experts per token), and how much cache each token of context takes.
//!
//! The header comes first in the file and holds the metadata and the
//! tensor list, so a few megabytes are enough, read from disk or fetched
//! from Hugging Face with a range request.

use std::{collections::HashMap, io::Read, path::Path};

#[derive(Clone, Debug, PartialEq)]
pub struct Info {
    pub name: Option<String>,
    pub architecture: String,
    pub params: u64,
    pub active: u64,
    /// The context the model was trained for.
    pub context: u32,
    /// Cache bytes per token of context in layers that see all of it, and
    /// in sliding-window layers, which keep only `window` tokens.
    pub cache_full: u64,
    pub cache_window: u64,
    pub window: u32,
    /// Parts of a model split across files; Solder runs single files.
    pub split: Option<u16>,
}

impl Info {
    /// Cache for `context` tokens (16-bit keys and values).
    pub fn cache(&self, context: u32) -> u64 {
        self.cache_full * context as u64
            + self.cache_window * context.min(self.window.max(1)) as u64
    }
}

#[derive(Debug, PartialEq)]
pub enum Error {
    /// The header goes on past the bytes given.
    Short,
    Bad(String),
}

#[derive(Clone, Debug)]
enum Value {
    Int(i128),
    Float(f64),
    Bool(bool),
    Str(String),
    Array(Vec<Value>),
    /// Long arrays of strings (the vocabulary) are skipped.
    Skipped,
}

impl Value {
    fn int(&self) -> Option<u64> {
        match self {
            Value::Int(i) => u64::try_from(*i).ok(),
            Value::Float(f) if *f >= 0. => Some(*f as u64),
            _ => None,
        }
    }

    /// A number, or one number per layer.
    fn per_layer(&self, layers: usize) -> Option<Vec<u64>> {
        match self {
            Value::Array(items) => items.iter().map(Value::int).collect(),
            Value::Int(_) => Some(vec![self.int()?; layers]),
            _ => None,
        }
    }
}

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self.pos.checked_add(n).ok_or(Error::Short)?;
        let bytes = self.data.get(self.pos..end).ok_or(Error::Short)?;
        self.pos = end;
        Ok(bytes)
    }

    fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn len(&mut self) -> Result<usize, Error> {
        let n = self.u64()?;
        // Nothing in a header is this long; a bad file, not a short read.
        if n > 1 << 32 {
            return Err(Error::Bad("not a GGUF file".into()));
        }
        Ok(n as usize)
    }

    fn string(&mut self) -> Result<String, Error> {
        let n = self.len()?;
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }

    fn value(&mut self, kind: u32) -> Result<Value, Error> {
        Ok(match kind {
            0 => Value::Int(self.take(1)?[0] as i128),
            1 => Value::Int(self.take(1)?[0] as i8 as i128),
            2 => Value::Int(u16::from_le_bytes(self.take(2)?.try_into().unwrap()) as i128),
            3 => Value::Int(i16::from_le_bytes(self.take(2)?.try_into().unwrap()) as i128),
            4 => Value::Int(self.u32()? as i128),
            5 => Value::Int(self.u32()? as i32 as i128),
            6 => Value::Float(f32::from_le_bytes(self.take(4)?.try_into().unwrap()) as f64),
            7 => Value::Bool(self.take(1)?[0] != 0),
            8 => Value::Str(self.string()?),
            9 => {
                let item = self.u32()?;
                let n = self.len()?;
                if item == 8 {
                    for _ in 0..n {
                        let len = self.len()?;
                        self.take(len)?;
                    }
                    Value::Skipped
                } else {
                    let mut items = Vec::with_capacity(n.min(4096));
                    for _ in 0..n {
                        items.push(self.value(item)?);
                    }
                    if n > 4096 {
                        Value::Skipped
                    } else {
                        Value::Array(items)
                    }
                }
            }
            10 => Value::Int(self.u64()? as i128),
            11 => Value::Int(self.u64()? as i64 as i128),
            12 => Value::Float(f64::from_le_bytes(self.take(8)?.try_into().unwrap())),
            _ => return Err(Error::Bad(format!("unknown GGUF value type {kind}"))),
        })
    }
}

/// Parses the header at the start of `data`.
pub fn parse(data: &[u8]) -> Result<Info, Error> {
    let mut c = Cursor { data, pos: 0 };
    if c.take(4)? != b"GGUF" {
        return Err(Error::Bad("not a GGUF file".into()));
    }
    let version = c.u32()?;
    if !(2..=3).contains(&version) {
        return Err(Error::Bad(format!(
            "GGUF version {version} is not supported"
        )));
    }
    let tensors = c.len()?;
    let kvs = c.len()?;
    let mut meta: HashMap<String, Value> = HashMap::new();
    for _ in 0..kvs {
        let key = c.string()?;
        let kind = c.u32()?;
        let value = c.value(kind)?;
        meta.insert(key, value);
    }
    let mut params = 0u64;
    let mut experts = 0u64;
    let mut lookups = 0u64;
    let mut embedding_table = 0u64;
    let mut has_output = false;
    for _ in 0..tensors {
        let name = c.string()?;
        let dims = c.u32()?;
        let mut count = 1u64;
        for _ in 0..dims {
            count = count.saturating_mul(c.u64()?);
        }
        c.take(4 + 8)?; // type and offset
        params += count;
        if name.contains("_exps") {
            experts += count;
        }
        // Embedding tables are looked up, one row per token, not read whole.
        if name.starts_with("per_layer_token_embd") {
            lookups += count;
        } else if name == "token_embd.weight" {
            lookups += count;
            embedding_table = count;
        }
        has_output |= name == "output.weight";
    }
    if !has_output {
        // The embedding doubles as the output layer, which reads all of it.
        lookups -= embedding_table;
    }

    let arch = match meta.get("general.architecture") {
        Some(Value::Str(s)) => s.clone(),
        _ => return Err(Error::Bad("the file names no architecture".into())),
    };
    let key = |k: &str| meta.get(&format!("{arch}.{k}"));
    let int = |k: &str| key(k).and_then(Value::int);
    let layers = int("block_count").unwrap_or(0) as usize;
    let embedding = int("embedding_length").unwrap_or(0);
    let heads = key("attention.head_count")
        .and_then(|v| v.per_layer(layers))
        .unwrap_or_default();
    let kv_heads = key("attention.head_count_kv")
        .and_then(|v| v.per_layer(layers))
        .unwrap_or_else(|| heads.clone());
    let head_dim = |layer: usize| {
        let h = heads.get(layer).copied().filter(|&h| h > 0).unwrap_or(1);
        embedding / h
    };
    let key_len = int("attention.key_length");
    let value_len = int("attention.value_length");

    // Hybrid models keep a cache only in every n-th layer; the rest are
    // recurrent, with a small fixed state.
    let interval = int("full_attention_interval").filter(|&n| n > 0);
    let window = int("attention.sliding_window").unwrap_or(0) as u32;
    let pattern = key("attention.sliding_window_pattern");
    let is_window_layer = |layer: usize| -> bool {
        if window == 0 {
            return false;
        }
        match pattern {
            Some(Value::Array(flags)) => matches!(flags.get(layer), Some(Value::Bool(true))),
            // Every n-th layer sees everything, the others a window.
            Some(v) => v
                .int()
                .is_some_and(|n| n > 0 && !(layer as u64 + 1).is_multiple_of(n)),
            None => false,
        }
    };
    let (mut cache_full, mut cache_window) = (0u64, 0u64);
    for layer in 0..layers {
        if interval.is_some_and(|n| !(layer as u64 + 1).is_multiple_of(n)) {
            continue;
        }
        let kv = kv_heads.get(layer).copied().unwrap_or(0);
        let k = key_len.unwrap_or_else(|| head_dim(layer));
        let v = value_len.unwrap_or_else(|| head_dim(layer));
        let bytes = kv * (k + v) * 2;
        if is_window_layer(layer) {
            cache_window += bytes;
        } else {
            cache_full += bytes;
        }
    }

    let used = int("expert_used_count").unwrap_or(0);
    let count = int("expert_count").unwrap_or(0);
    let active_experts = if count > 0 {
        experts * used / count
    } else {
        experts
    };
    let active = (params - experts - lookups.min(params - experts)) + active_experts;
    Ok(Info {
        name: match meta.get("general.name") {
            Some(Value::Str(s)) => Some(s.clone()),
            _ => None,
        },
        architecture: arch.clone(),
        params,
        active: active.max(1),
        context: int("context_length").unwrap_or(4096).min(u32::MAX as u64) as u32,
        cache_full,
        cache_window,
        window,
        split: meta
            .get("split.count")
            .and_then(Value::int)
            .filter(|&n| n > 1)
            .map(|n| n as u16),
    })
}

/// Reads the header of a file on disk.
pub fn read_file(path: &Path) -> Result<Info, String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut data = Vec::new();
    let mut chunk = 4 << 20;
    loop {
        let start = data.len();
        data.resize(start + chunk, 0);
        let n = file.read(&mut data[start..]).map_err(|e| e.to_string())?;
        // `read` may return less than asked; keep reading until the file ends.
        let mut filled = n;
        while filled < chunk {
            let more = file
                .read(&mut data[start + filled..])
                .map_err(|e| e.to_string())?;
            if more == 0 {
                break;
            }
            filled += more;
        }
        data.truncate(start + filled);
        match parse(&data) {
            Ok(info) => return Ok(info),
            Err(Error::Bad(e)) => return Err(e),
            Err(Error::Short) if filled < chunk => {
                return Err("The file ends inside its header".into());
            }
            Err(Error::Short) if data.len() > 256 << 20 => {
                return Err("The header is too large".into());
            }
            Err(Error::Short) => chunk *= 2,
        }
    }
}

/// Reads the header of a file on Hugging Face with range requests.
pub async fn read_remote(url: &str) -> Result<Info, String> {
    let mut len: u64 = 4 << 20;
    loop {
        let response = crate::client()
            .get(url)
            .header("Range", format!("bytes=0-{}", len - 1))
            .send()
            .await
            .map_err(|e| format!("Could not reach Hugging Face: {e}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "Could not read the model: HTTP {}",
                response.status().as_u16()
            ));
        }
        let full = response.status() == reqwest::StatusCode::OK;
        let data = response.bytes().await.map_err(|e| e.to_string())?;
        match parse(&data) {
            Ok(info) => return Ok(info),
            Err(Error::Bad(e)) => return Err(e),
            Err(Error::Short) if full || (data.len() as u64) < len => {
                return Err("The file ends inside its header".into());
            }
            Err(Error::Short) if len >= 128 << 20 => return Err("The header is too large".into()),
            Err(Error::Short) => len *= 4,
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// Writes a GGUF header: metadata and tensors (name and dimensions).
    pub fn header(meta: &[(&str, MetaValue)], tensors: &[(&str, &[u64])]) -> Vec<u8> {
        let mut out = b"GGUF".to_vec();
        out.extend(3u32.to_le_bytes());
        out.extend((tensors.len() as u64).to_le_bytes());
        out.extend((meta.len() as u64).to_le_bytes());
        let string = |out: &mut Vec<u8>, s: &str| {
            out.extend((s.len() as u64).to_le_bytes());
            out.extend(s.as_bytes());
        };
        for (key, value) in meta {
            string(&mut out, key);
            match value {
                MetaValue::U32(v) => {
                    out.extend(4u32.to_le_bytes());
                    out.extend(v.to_le_bytes());
                }
                MetaValue::Str(s) => {
                    out.extend(8u32.to_le_bytes());
                    string(&mut out, s);
                }
                MetaValue::U32s(items) => {
                    out.extend(9u32.to_le_bytes());
                    out.extend(4u32.to_le_bytes());
                    out.extend((items.len() as u64).to_le_bytes());
                    for i in *items {
                        out.extend(i.to_le_bytes());
                    }
                }
                MetaValue::Bools(items) => {
                    out.extend(9u32.to_le_bytes());
                    out.extend(7u32.to_le_bytes());
                    out.extend((items.len() as u64).to_le_bytes());
                    for b in *items {
                        out.push(*b as u8);
                    }
                }
                MetaValue::Strs(items) => {
                    out.extend(9u32.to_le_bytes());
                    out.extend(8u32.to_le_bytes());
                    out.extend((items.len() as u64).to_le_bytes());
                    for s in *items {
                        string(&mut out, s);
                    }
                }
            }
        }
        for (name, dims) in tensors {
            string(&mut out, name);
            out.extend((dims.len() as u32).to_le_bytes());
            for d in *dims {
                out.extend(d.to_le_bytes());
            }
            out.extend(0u32.to_le_bytes());
            out.extend(0u64.to_le_bytes());
        }
        out
    }

    pub enum MetaValue {
        U32(u32),
        Str(&'static str),
        U32s(&'static [u32]),
        Bools(&'static [bool]),
        Strs(&'static [&'static str]),
    }
    use MetaValue::*;

    #[test]
    fn dense_model() {
        let data = header(
            &[
                ("general.architecture", Str("llama")),
                ("general.name", Str("Tiny")),
                ("tokenizer.ggml.tokens", Strs(&["a", "b", "c"])),
                ("llama.block_count", U32(2)),
                ("llama.context_length", U32(8192)),
                ("llama.embedding_length", U32(64)),
                ("llama.attention.head_count", U32(8)),
                ("llama.attention.head_count_kv", U32(2)),
            ],
            &[
                ("token_embd.weight", &[64, 100]),
                ("blk.0.attn_q.weight", &[64, 64]),
                ("blk.1.attn_q.weight", &[64, 64]),
                ("output.weight", &[64, 100]),
            ],
        );
        let info = parse(&data).unwrap();
        assert_eq!(info.name.as_deref(), Some("Tiny"));
        assert_eq!(info.architecture, "llama");
        assert_eq!(info.params, 6400 + 4096 * 2 + 6400);
        // The embedding is a lookup; the output layer is read whole.
        assert_eq!(info.active, 4096 * 2 + 6400);
        assert_eq!(info.context, 8192);
        // 2 layers x 2 KV heads x (8 + 8) dims x 2 bytes.
        assert_eq!(info.cache_full, 2 * 2 * 16 * 2);
        assert_eq!(info.cache(1000), 128_000);
        assert_eq!(info.split, None);
        // Every prefix is reported as short, never as a bad file.
        for n in [0, 3, 10, 40, data.len() - 1] {
            assert_eq!(parse(&data[..n]), Err(Error::Short), "{n}");
        }
        assert!(matches!(parse(b"GGML...."), Err(Error::Bad(_))));
    }

    #[test]
    fn experts_hybrid_layers_and_windows() {
        let moe = header(
            &[
                ("general.architecture", Str("qwen3moe")),
                ("qwen3moe.block_count", U32(4)),
                ("qwen3moe.embedding_length", U32(32)),
                ("qwen3moe.attention.head_count", U32(4)),
                ("qwen3moe.attention.head_count_kv", U32(1)),
                ("qwen3moe.attention.key_length", U32(16)),
                ("qwen3moe.attention.value_length", U32(16)),
                ("qwen3moe.expert_count", U32(8)),
                ("qwen3moe.expert_used_count", U32(2)),
                ("qwen3moe.full_attention_interval", U32(4)),
                ("split.count", U32(3)),
            ],
            &[
                ("blk.0.attn_q.weight", &[100, 10]),
                ("blk.0.ffn_up_exps.weight", &[10, 10, 8]),
                ("output.weight", &[10, 10]),
            ],
        );
        let info = parse(&moe).unwrap();
        assert_eq!(info.params, 1000 + 800 + 100);
        assert_eq!(info.active, 1000 + 200 + 100);
        // Only every fourth layer keeps a cache: 1 layer x 1 head x 32 x 2.
        assert_eq!(info.cache_full, 64);
        assert_eq!(info.split, Some(3));

        let gemma = header(
            &[
                ("general.architecture", Str("gemma4")),
                ("gemma4.block_count", U32(4)),
                ("gemma4.embedding_length", U32(32)),
                ("gemma4.attention.head_count", U32s(&[4, 4, 4, 4])),
                ("gemma4.attention.head_count_kv", U32s(&[1, 1, 1, 0])),
                ("gemma4.attention.sliding_window", U32(512)),
                (
                    "gemma4.attention.sliding_window_pattern",
                    Bools(&[true, true, false, true]),
                ),
            ],
            &[("blk.0.attn_q.weight", &[8, 8])],
        );
        let info = parse(&gemma).unwrap();
        // Head dim 8: each attention layer takes 1 x 16 x 2 bytes a token.
        assert_eq!(info.cache_full, 32);
        assert_eq!(info.cache_window, 64);
        assert_eq!(info.cache(10_000), 32 * 10_000 + 64 * 512);
    }

    #[test]
    fn reads_files_in_growing_chunks() {
        let dir = std::env::temp_dir().join(format!("solder-gguf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A vocabulary larger than the first chunk.
        static WORDS: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
        let words = WORDS.get_or_init(|| {
            (0..300_000)
                .map(|i| &*Box::leak(format!("token{i:08}").into_boxed_str()))
                .collect()
        });
        let big: &'static [&'static str] = Box::leak(words.clone().into_boxed_slice());
        let data = header(
            &[
                ("general.architecture", Str("llama")),
                ("tokenizer.ggml.tokens", Strs(big)),
                ("llama.block_count", U32(1)),
            ],
            &[("output.weight", &[4, 4])],
        );
        assert!(data.len() > 4 << 20);
        let path = dir.join("big.gguf");
        std::fs::write(&path, &data).unwrap();
        assert_eq!(read_file(&path).unwrap().params, 16);
        std::fs::write(&path, &data[..data.len() - 5]).unwrap();
        assert!(read_file(&path).is_err());
    }
}
