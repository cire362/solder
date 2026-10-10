//! Jupyter's file format, independent of any kernel or extension.
//! Keep the original dictionaries: metadata, attachments and output forms
//! we cannot draw must survive an edit and a save.
use serde_json::{Value, json};
use std::{collections::HashMap, path::Path};

const LIMIT: u64 = 64 * 1024 * 1024;

#[derive(Clone)]
pub struct File {
    original: Value,
    cells: HashMap<u64, Value>,
}

impl File {
    pub fn read(path: &Path) -> Result<(Self, Value), String> {
        if std::fs::metadata(path).map_err(|e| e.to_string())?.len() > LIMIT {
            return Err("This notebook is larger than 64 MB".into());
        }
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        Self::decode(&bytes)
    }

    fn decode(bytes: &[u8]) -> Result<(Self, Value), String> {
        let original: Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        if original["nbformat"] != 4 {
            return Err("This notebook needs Jupyter format 4".into());
        }
        let cells = original["cells"]
            .as_array()
            .ok_or("The notebook has no cells")?;
        let language = original["metadata"]["language_info"]["name"]
            .as_str()
            .or(original["metadata"]["kernelspec"]["language"].as_str())
            .unwrap_or("python");
        let mut shown = Vec::new();
        let mut kept = HashMap::new();
        for (handle, cell) in cells.iter().enumerate() {
            let kind = cell["cell_type"].as_str().ok_or("A cell has no kind")?;
            let value = multiline(&cell["source"]).ok_or("A cell has invalid source text")?;
            let code = kind == "code";
            shown.push(json!({"handle":handle, "code":code,
                "language":if code {language} else if kind == "markdown" {"markdown"} else {"plaintext"},
                "value":value, "order":cell["execution_count"],
                "outputs":cell["outputs"].as_array().map(|outputs| outputs.iter().map(output).collect::<Vec<_>>()).unwrap_or_default()}));
            kept.insert(handle as u64, cell.clone());
        }
        Ok((
            Self {
                original,
                cells: kept,
            },
            json!({"cells":shown}),
        ))
    }

    /// The cells in their current order. A handle keeps the original cell
    /// even after it moves; an added cell gets a new, unique Jupyter id.
    pub fn content(&self, cells: &[(u64, bool, String)]) -> Value {
        let mut result = self.original.clone();
        let mut ids: std::collections::HashSet<String> = self
            .cells
            .values()
            .filter_map(|cell| cell["id"].as_str().map(str::to_string))
            .collect();
        result["cells"] = cells.iter().map(|(handle, code, source)| {
            let mut cell = self.cells.get(handle).cloned().unwrap_or_else(|| {
                let mut id = format!("solder_{handle}");
                while !ids.insert(id.clone()) { id.push('_'); }
                if *code {
                    json!({"cell_type":"code","id":id,"metadata":{},"execution_count":null,"outputs":[],"source":""})
                } else {
                    json!({"cell_type":"markdown","id":id,"metadata":{},"source":""})
                }
            });
            if multiline(&cell["source"]).as_deref() != Some(source) {
                cell["source"] = source.clone().into();
            }
            cell
        }).collect::<Vec<_>>().into();
        result
    }

    pub fn save(&self, path: &Path, cells: &[(u64, bool, String)]) -> Result<(), String> {
        let mut bytes =
            serde_json::to_vec_pretty(&self.content(cells)).map_err(|e| e.to_string())?;
        bytes.push(b'\n');
        write_atomic(path, &bytes)
    }
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let parent = path.parent().ok_or("This file has no folder")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let temporary = parent.join(format!(
        ".solder-save-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        if let Ok(metadata) = std::fs::metadata(path) {
            file.set_permissions(metadata.permissions())?;
        }
        std::fs::rename(&temporary, path)
    })();
    let _ = std::fs::remove_file(&temporary);
    result.map_err(|e| e.to_string())
}

fn multiline(value: &Value) -> Option<String> {
    if let Some(text) = value.as_str() {
        return Some(text.to_string());
    }
    value
        .as_array()?
        .iter()
        .map(|line| line.as_str())
        .collect::<Option<Vec<_>>>()
        .map(|lines| lines.concat())
}

fn output(value: &Value) -> Value {
    match value["output_type"].as_str() {
        Some("stream") => {
            json!({"items":[{"mime":if value["name"] == "stderr" {"application/vnd.code.notebook.stderr"} else {"application/vnd.code.notebook.stdout"},"text":multiline(&value["text"]).unwrap_or_default()}]})
        }
        Some("error") => {
            json!({"items":[{"mime":"application/vnd.code.notebook.error","text": value["traceback"].as_array().map(|lines| lines.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\n")).filter(|text| !text.is_empty()).unwrap_or_else(|| format!("{}: {}",value["ename"].as_str().unwrap_or("Error"),value["evalue"].as_str().unwrap_or_default()))}]})
        }
        Some("display_data" | "execute_result") => {
            let items: Vec<Value> = value["data"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(mime, data)| {
                    let text = multiline(data).unwrap_or_else(|| data.to_string());
                    if matches!(
                        mime.as_str(),
                        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
                    ) {
                        json!({"mime":mime,"picture":text,"size":text.len() * 3 / 4})
                    } else {
                        json!({"mime":mime,"text":text,"size":text.len()})
                    }
                })
                .collect();
            json!({"items":items})
        }
        _ => json!({"items":[{"mime":value["output_type"],"size":value.to_string().len()}]}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jupyter_edit_keeps_metadata_outputs_attachments_and_cell_identity() {
        let source = json!({"nbformat":4,"nbformat_minor":5,"extra":{"future":true},
            "metadata":{"kernelspec":{"language":"julia"},"custom":42},"cells":[
            {"cell_type":"markdown","source":["# Title\n","![a](attachment:a)"],"id":"a","metadata":{"custom":9},"attachments":{"a":{"image/png":"AA=="}}},
            {"cell_type":"code","source":"2 + 2","id":"b","metadata":{},"execution_count":7,"outputs":[{"output_type":"display_data","data":{"text/html":["<b>","4</b>"],"application/json":{"x":4}},"metadata":{"custom":1}}]},
            {"cell_type":"raw","source":"latex","id":"c","metadata":{"format":"latex"}}
        ]});
        let (file, shown) = File::decode(&serde_json::to_vec(&source).unwrap()).unwrap();
        assert_eq!(shown["cells"][1]["language"], "julia");
        assert_eq!(
            shown["cells"][1]["outputs"][0]["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["mime"] == "text/html")
                .unwrap()["text"],
            "<b>4</b>"
        );
        let saved = file.content(&[
            (1, true, "3 + 3".into()),
            (0, false, "# New".into()),
            (2, false, "latex".into()),
            (3, true, "print(1)".into()),
        ]);
        assert_eq!(saved["metadata"], source["metadata"]);
        assert_eq!(saved["extra"], source["extra"]);
        assert_eq!(saved["cells"][0]["outputs"], source["cells"][1]["outputs"]);
        assert_eq!(saved["cells"][0]["id"], "b");
        assert_eq!(
            saved["cells"][1]["attachments"],
            source["cells"][0]["attachments"]
        );
        assert_eq!(saved["cells"][2], source["cells"][2]);
        assert_eq!(saved["cells"][3]["execution_count"], Value::Null);
        assert!(saved["cells"][3]["id"].is_string());
    }

    #[test]
    fn invalid_notebooks_are_rejected_before_any_save() {
        for source in [
            json!({"nbformat":3}),
            json!({"nbformat":4}),
            json!({"nbformat":4,"cells":[{"cell_type":"code","source":[42]}]}),
        ] {
            assert!(File::decode(&serde_json::to_vec(&source).unwrap()).is_err());
        }
    }
}
