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
    stamp: std::sync::Arc<std::sync::Mutex<Option<crate::remote::Stamp>>>,
}

impl File {
    pub fn read(path: &Path) -> Result<(Self, Value), String> {
        if crate::remote::metadata(path)
            .map_err(|e| e.to_string())?
            .len()
            > LIMIT
        {
            return Err(crate::lang::t("This notebook is larger than 64 MB").into());
        }
        let (bytes, stamp) = crate::remote::read_versioned(path).map_err(|e| e.to_string())?;
        let (file, shown) = Self::decode(&bytes)?;
        *file.stamp.lock().unwrap_or_else(|e| e.into_inner()) = stamp;
        Ok((file, shown))
    }

    fn decode(bytes: &[u8]) -> Result<(Self, Value), String> {
        let original: Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        if original["nbformat"] != 4 {
            return Err(crate::lang::t("This notebook needs Jupyter format 4").into());
        }
        let cells = original["cells"]
            .as_array()
            .ok_or(crate::lang::t("The notebook has no cells"))?;
        let language = original["metadata"]["language_info"]["name"]
            .as_str()
            .or(original["metadata"]["kernelspec"]["language"].as_str())
            .unwrap_or("python");
        let mut shown = Vec::new();
        let mut kept = HashMap::new();
        for (handle, cell) in cells.iter().enumerate() {
            let kind = cell["cell_type"]
                .as_str()
                .ok_or(crate::lang::t("A cell has no kind"))?;
            let value = multiline(&cell["source"])
                .ok_or(crate::lang::t("A cell has invalid source text"))?;
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
                stamp: Default::default(),
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
        let mut stamp = self.stamp.lock().unwrap_or_else(|e| e.into_inner());
        *stamp = crate::remote::write_versioned(path, &bytes, stamp.as_ref())
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or(crate::lang::t("This file has no folder"))?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    crate::file_save::replace(path, bytes)
        .map(|_| ())
        .map_err(|e| e.to_string())
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
            json!({"items":[{"mime":"application/vnd.code.notebook.error","text": value["traceback"].as_array().map(|lines| lines.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\n")).filter(|text| !text.is_empty()).unwrap_or_else(|| format!("{}: {}",value["ename"].as_str().unwrap_or(crate::lang::t("Error")),value["evalue"].as_str().unwrap_or_default()))}]})
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

    fn notebook_path(name: &str) -> std::path::PathBuf {
        let path = db::testing::dir(name).join("test.ipynb");
        let source = json!({"nbformat":4,"nbformat_minor":5,"metadata":{},"cells":[
            {"cell_type":"code","id":"a","metadata":{},"source":"old","execution_count":null,"outputs":[]}
        ]});
        std::fs::write(&path, serde_json::to_vec(&source).unwrap()).unwrap();
        path
    }

    #[test]
    fn notebook_save_rejects_external_edit_with_the_same_length_and_mtime() {
        let path = notebook_path("ipynb-external-edit");
        let (file, _) = File::read(&path).unwrap();
        let stamp = file.stamp.lock().unwrap().clone();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let original = std::fs::read_to_string(&path).unwrap();
        let external = original.replace("old", "new");
        assert_eq!(external.len(), original.len());
        std::fs::write(&path, &external).unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            modified
        );

        for _ in 0..2 {
            assert!(file.save(&path, &[(0, true, "ours".into())]).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), external.as_bytes());
            assert_eq!(*file.stamp.lock().unwrap(), stamp);
        }
    }

    #[test]
    fn independently_loaded_notebooks_conflict_and_success_advances_the_revision() {
        let path = notebook_path("ipynb-independent-copies");
        let (first, _) = File::read(&path).unwrap();
        let (second, _) = File::read(&path).unwrap();
        let second_stamp = second.stamp.lock().unwrap().clone();
        first.save(&path, &[(0, true, "first".into())]).unwrap();
        let saved = std::fs::read(&path).unwrap();
        assert!(second.save(&path, &[(0, true, "second".into())]).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), saved);
        assert_eq!(*second.stamp.lock().unwrap(), second_stamp);

        first.save(&path, &[(0, true, "latest".into())]).unwrap();
        let (_, shown) = File::read(&path).unwrap();
        assert_eq!(shown["cells"][0]["value"], "latest");
    }

    #[test]
    fn a_notebook_without_a_read_revision_only_creates_a_new_file() {
        let path = notebook_path("ipynb-create-only");
        let original = std::fs::read(&path).unwrap();
        let (file, _) = File::decode(&original).unwrap();
        assert!(file.save(&path, &[(0, true, "ours".into())]).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(file.stamp.lock().unwrap().is_none());

        let created = path.with_file_name("new.ipynb");
        file.save(&created, &[(0, true, "created".into())]).unwrap();
        assert!(file.stamp.lock().unwrap().is_some());
        file.save(&created, &[(0, true, "updated".into())]).unwrap();
        let (_, shown) = File::read(&created).unwrap();
        assert_eq!(shown["cells"][0]["value"], "updated");
    }

    #[cfg(unix)]
    #[test]
    fn failed_notebook_replacement_keeps_bytes_and_read_revision_for_a_retry() {
        let path = notebook_path("ipynb-failed-replacement");
        let original = std::fs::read(&path).unwrap();
        let (file, _) = File::read(&path).unwrap();
        let stamp = file.stamp.lock().unwrap().clone();
        let linked = path.with_file_name("linked.ipynb");
        std::fs::hard_link(&path, &linked).unwrap();

        assert!(file.save(&path, &[(0, true, "ours".into())]).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(std::fs::read(&linked).unwrap(), original);
        assert_eq!(*file.stamp.lock().unwrap(), stamp);

        std::fs::remove_file(linked).unwrap();
        file.save(&path, &[(0, true, "ours".into())]).unwrap();
        let (_, shown) = File::read(&path).unwrap();
        assert_eq!(shown["cells"][0]["value"], "ours");
    }

    #[test]
    fn state_replacement_creates_parent_folders_and_replaces_existing_bytes() {
        let path = db::testing::dir("ipynb-state-replacement").join("nested/state.json");
        write_atomic(&path, b"first").unwrap();
        write_atomic(&path, b"second").unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"second");
    }

    #[cfg(unix)]
    #[test]
    fn failed_state_replacement_leaves_the_original_and_its_hardlink() {
        let dir = db::testing::dir("ipynb-state-hardlink");
        let path = dir.join("state.json");
        let linked = dir.join("linked.json");
        std::fs::write(&path, b"original").unwrap();
        std::fs::hard_link(&path, &linked).unwrap();
        assert!(write_atomic(&path, b"replacement").is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"original");
        assert_eq!(std::fs::read(linked).unwrap(), b"original");
        assert_eq!(std::fs::read_dir(dir).unwrap().count(), 2);
    }

    #[test]
    fn localized_validation_keeps_the_jupyter_file_and_cell_identifiers() {
        crate::lang::set(crate::lang::Language::Russian);
        let invalid = json!({"nbformat":3});
        assert_eq!(
            File::decode(&serde_json::to_vec(&invalid).unwrap())
                .err()
                .unwrap(),
            "Для этого блокнота нужен формат Jupyter 4"
        );
        let source = json!({"nbformat":4,"nbformat_minor":5,"metadata":{},"cells":[
            {"cell_type":"markdown","id":"source","metadata":{},"source":"# Original content"}
        ]});
        let (file, shown) = File::decode(&serde_json::to_vec(&source).unwrap()).unwrap();
        assert_eq!(shown["cells"][0]["language"], "markdown");
        assert_eq!(shown["cells"][0]["value"], "# Original content");
        assert_eq!(
            file.content(&[(0, false, "# Original content".into())]),
            source
        );
        crate::lang::set(crate::lang::Language::English);
        assert_eq!(
            File::decode(&serde_json::to_vec(&invalid).unwrap())
                .err()
                .unwrap(),
            "This notebook needs Jupyter format 4"
        );
    }

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
