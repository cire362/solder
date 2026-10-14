//! Jupyter's file format, independent of any kernel or extension.
//! Keep the original dictionaries: metadata, attachments and output forms
//! we cannot draw must survive an edit and a save.
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, TryLockError},
};

use crate::remote::Stamp;

const LIMIT: u64 = 64 * 1024 * 1024;

#[derive(Clone)]
pub struct File {
    original: Arc<Value>,
    source: Option<PathBuf>,
    cells: Arc<HashMap<u64, Value>>,
    stamp: Arc<Mutex<Option<Stamp>>>,
}

#[derive(Clone)]
pub struct Checkpoint {
    file: File,
    stamp: Option<Stamp>,
}

impl Checkpoint {
    /// Builds the complete recovery copy on the writer or a background thread.
    pub fn snapshot(&self, cells: &[(u64, bool, String)]) -> Value {
        json!({"version":1, "content":self.file.content(cells), "stamp":self.stamp,
            "source":self.file.source.as_deref().and_then(Path::to_str)})
    }
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
        let source = crate::remote::canonicalize(path).map_err(|error| error.to_string())?;
        let (bytes, stamp) = crate::remote::read_versioned(&source).map_err(|e| e.to_string())?;
        let (mut file, shown) = Self::decode(&bytes)?;
        if crate::remote::canonicalize(path).map_err(|error| error.to_string())? != source {
            return Err(crate::lang::t("The notebook path changed while it was read").into());
        }
        file.source = Some(source);
        *file.stamp.lock().unwrap_or_else(|e| e.into_inner()) = stamp;
        Ok((file, shown))
    }

    fn decode(bytes: &[u8]) -> Result<(Self, Value), String> {
        let original: Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        Self::from_value(original)
    }

    fn from_value(original: Value) -> Result<(Self, Value), String> {
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
                original: Arc::new(original),
                source: None,
                cells: Arc::new(kept),
                stamp: Default::default(),
            },
            json!({"cells":shown}),
        ))
    }

    pub(crate) fn original_source(&self) -> Option<&Path> {
        self.source.as_deref()
    }

    /// Captures the read revision without waiting for a save to finish its IO.
    pub fn checkpoint(&self) -> Option<Checkpoint> {
        let stamp = match self.stamp.try_lock() {
            Ok(stamp) => stamp.clone(),
            Err(TryLockError::Poisoned(error)) => error.into_inner().clone(),
            Err(TryLockError::WouldBlock) => return None,
        };
        Some(Checkpoint {
            file: self.clone(),
            stamp,
        })
    }

    /// Restores only the backed-up content and revision, without reading a source.
    pub fn restore(snapshot: &Value) -> Result<(Self, Value), String> {
        let invalid = crate::lang::t("This notebook recovery copy is invalid");
        let version = snapshot["version"].as_u64().ok_or(invalid)?;
        if version != 1 {
            return Err(crate::lang::t("This notebook recovery version is not supported").into());
        }
        let original = snapshot.get("content").ok_or(invalid)?;
        let stamp: Option<Stamp> =
            serde_json::from_value(snapshot.get("stamp").ok_or(invalid)?.clone())
                .map_err(|_| invalid.to_string())?;
        let (mut file, shown) = Self::from_value(original.clone())?;
        file.source = match snapshot.get("source") {
            Some(Value::String(path)) if Path::new(path).is_absolute() => Some(PathBuf::from(path)),
            None | Some(Value::Null) => None,
            _ => return Err(invalid.into()),
        };
        let stamp = stamp.filter(|stamp| {
            stamp.hash.as_ref().is_some_and(|hash| {
                hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        });
        *file.stamp.lock().unwrap_or_else(|e| e.into_inner()) = stamp;
        Ok((file, shown))
    }

    /// The cells in their current order. A handle keeps the original cell
    /// even after it moves; an added cell gets a new, unique Jupyter id.
    pub fn content(&self, cells: &[(u64, bool, String)]) -> Value {
        let mut result = self.original.as_ref().clone();
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
        let bytes = self.bytes(cells)?;
        let mut stamp = self.stamp.lock().unwrap_or_else(|e| e.into_inner());
        *stamp = crate::remote::write_versioned(path, &bytes, stamp.as_ref())
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Writes a copy against the destination's revision, keeping the source's.
    /// The caller checks source aliases before choosing the expected revision.
    pub fn export(
        &self,
        path: &Path,
        cells: &[(u64, bool, String)],
        expected: Option<&Stamp>,
    ) -> Result<(), String> {
        crate::remote::write_versioned(path, &self.bytes(cells)?, expected)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn bytes(&self, cells: &[(u64, bool, String)]) -> Result<Vec<u8>, String> {
        let mut bytes =
            serde_json::to_vec_pretty(&self.content(cells)).map_err(|e| e.to_string())?;
        bytes.push(b'\n');
        Ok(bytes)
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

    fn rich_notebook() -> Value {
        json!({"nbformat":4,"nbformat_minor":5,"extra":{"future":true},
            "metadata":{"language_info":{"name":"julia"},"kernelspec":{"language":"python"},"custom":{"nested":[1,2]}},"cells":[
            {"cell_type":"markdown","source":["# Title\n","![a](attachment:a)"],"id":"a","metadata":{"custom":9},"attachments":{"a":{"image/png":"AA==","image/svg+xml":["<svg>","</svg>"]}}},
            {"cell_type":"code","source":["2 + 2\n"],"id":"b","metadata":{"tags":["keep"]},"execution_count":7,"outputs":[
                {"output_type":"stream","name":"stdout","text":["hello\n","world"]},
                {"output_type":"error","ename":"Error","evalue":"detail","traceback":["first","second"]},
                {"output_type":"display_data","data":{"text/html":["<b>","4</b>"],"image/png":"AA==","application/json":{"x":4}},"metadata":{"custom":1}},
                {"output_type":"execute_result","execution_count":7,"data":{"text/plain":["4"],"application/custom":{"future":[true]}},"metadata":{"custom":2}},
                {"output_type":"future","payload":{"keep":[1,2,3]}}
            ]},
            {"cell_type":"raw","source":["latex\n"],"id":"solder_3","metadata":{"format":"latex"},"custom":{"keep":true}}
        ]})
    }

    fn shown_cells(shown: &Value) -> Vec<(u64, bool, String)> {
        shown["cells"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cell| {
                (
                    cell["handle"].as_u64().unwrap(),
                    cell["code"].as_bool().unwrap(),
                    cell["value"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }

    #[test]
    fn file_and_checkpoint_clones_share_the_original_model() {
        let (file, _) = File::decode(&serde_json::to_vec(&rich_notebook()).unwrap()).unwrap();
        let cloned = file.clone();
        let checkpoint = file.checkpoint().unwrap();
        let captured = checkpoint.clone();
        for other in [&cloned, &checkpoint.file, &captured.file] {
            assert!(Arc::ptr_eq(&file.original, &other.original));
            assert!(Arc::ptr_eq(&file.cells, &other.cells));
            assert!(Arc::ptr_eq(&file.stamp, &other.stamp));
        }
    }

    #[test]
    fn checkpoint_does_not_wait_for_a_save_holding_the_stamp() {
        let path = notebook_path("ipynb-checkpoint-busy");
        let (file, _) = File::read(&path).unwrap();
        let checkpoint = file.checkpoint().unwrap();
        let saving = file.stamp.lock().unwrap();
        assert!(file.checkpoint().is_none());
        assert_eq!(
            checkpoint.snapshot(&[(0, true, "recovered".into())])["stamp"],
            json!(*saving)
        );
        drop(saving);
        assert!(file.checkpoint().is_some());
    }

    #[test]
    fn checkpoints_keep_the_captured_revision_after_saves_reorders_additions_and_deletions() {
        let path = notebook_path("ipynb-checkpoint-immutable");
        std::fs::write(&path, serde_json::to_vec(&rich_notebook()).unwrap()).unwrap();
        let (file, _) = File::read(&path).unwrap();
        let checkpoint = file.checkpoint().unwrap();
        let cloned = checkpoint.clone();
        let stamp = file.stamp.lock().unwrap().clone();
        let cells = vec![
            (1, true, "3 + 3".into()),
            (0, false, "# Recovered".into()),
            (3, true, "print(1)".into()),
        ];
        let content = file.content(&cells);
        assert_eq!(content["cells"][2]["id"], "solder_3_");
        let expected = json!({"version":1,"content":content,"stamp":stamp,"source":file.source});
        assert_eq!(checkpoint.snapshot(&cells), expected);

        let later = file.clone();
        later
            .save(
                &path,
                &[
                    (2, false, "later raw".into()),
                    (4, false, "added note".into()),
                    (0, false, "# Later".into()),
                ],
            )
            .unwrap();
        later
            .save(&path, &[(4, false, "latest note".into())])
            .unwrap();
        assert_ne!(*file.stamp.lock().unwrap(), stamp);
        assert_eq!(checkpoint.snapshot(&cells), expected);
        assert_eq!(cloned.snapshot(&cells), expected);
        assert_eq!(
            file.checkpoint().unwrap().stamp,
            *file.stamp.lock().unwrap()
        );
    }

    #[test]
    fn recovery_and_export_preserve_metadata_attachments_outputs_and_cell_identity() {
        let path = notebook_path("ipynb-recovery-fidelity");
        let original = rich_notebook();
        let bytes = serde_json::to_vec(&original).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let (file, _) = File::read(&path).unwrap();
        let cells = vec![
            (2, false, "latex\n".into()),
            (1, true, "3 + 3".into()),
            (0, false, "# Recovered".into()),
            (3, true, "print(1)".into()),
            (4, false, "new note".into()),
        ];
        let snapshot = file.checkpoint().unwrap().snapshot(&cells);
        let stamp = file.stamp.lock().unwrap().clone();
        let content = &snapshot["content"];
        assert_eq!(content["metadata"], original["metadata"]);
        assert_eq!(content["extra"], original["extra"]);
        assert_eq!(content["cells"][0], original["cells"][2]);
        assert_eq!(
            content["cells"][1]["outputs"],
            original["cells"][1]["outputs"]
        );
        assert_eq!(
            content["cells"][1]["metadata"],
            original["cells"][1]["metadata"]
        );
        assert_eq!(content["cells"][1]["execution_count"], 7);
        assert_eq!(content["cells"][1]["id"], "b");
        assert_eq!(
            content["cells"][2]["attachments"],
            original["cells"][0]["attachments"]
        );
        assert_eq!(
            content["cells"][2]["metadata"],
            original["cells"][0]["metadata"]
        );
        assert_eq!(content["cells"][2]["id"], "a");
        assert_eq!(content["cells"][3]["id"], "solder_3_");
        assert_eq!(content["cells"][3]["outputs"], json!([]));
        assert_eq!(content["cells"][3]["execution_count"], Value::Null);
        assert_eq!(content["cells"][4]["cell_type"], "markdown");

        let backup: Value =
            serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
        let (restored, shown) = File::restore(&backup).unwrap();
        assert_eq!(*restored.stamp.lock().unwrap(), stamp);
        assert_eq!(shown["cells"][0]["language"], "plaintext");
        assert_eq!(shown["cells"][1]["language"], "julia");
        assert_eq!(shown["cells"][1]["value"], "3 + 3");
        assert_eq!(shown["cells"][1]["order"], 7);
        assert_eq!(shown["cells"][1]["outputs"].as_array().unwrap().len(), 5);
        assert_eq!(
            shown["cells"][1]["outputs"][0]["items"][0]["text"],
            "hello\nworld"
        );
        assert_eq!(
            shown["cells"][1]["outputs"][1]["items"][0]["text"],
            "first\nsecond"
        );
        let cells = shown_cells(&shown);
        assert_eq!(restored.content(&cells), *content);
        assert_eq!(restored.checkpoint().unwrap().snapshot(&cells), snapshot);

        let exported = path.with_file_name("exported.ipynb");
        restored.export(&exported, &cells, None).unwrap();
        let exported_bytes = std::fs::read(&exported).unwrap();
        assert_eq!(exported_bytes.last(), Some(&b'\n'));
        assert_eq!(
            serde_json::from_slice::<Value>(&exported_bytes).unwrap(),
            *content
        );
        let (_, exported_shown) = File::read(&exported).unwrap();
        assert_eq!(exported_shown, shown);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(*restored.stamp.lock().unwrap(), stamp);

        restored.save(&path, &cells).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&std::fs::read(&path).unwrap()).unwrap(),
            *content
        );
    }

    #[test]
    fn restored_notebooks_keep_the_backup_revision_when_the_source_changed() {
        let path = notebook_path("ipynb-recovery-conflict");
        let (file, _) = File::read(&path).unwrap();
        let snapshot = file
            .checkpoint()
            .unwrap()
            .snapshot(&[(0, true, "recovered".into())]);
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
        let (live, _) = File::read(&path).unwrap();
        assert_ne!(*live.stamp.lock().unwrap(), stamp);

        let (restored, shown) = File::restore(&snapshot).unwrap();
        let cells = shown_cells(&shown);
        assert_eq!(shown["cells"][0]["value"], "recovered");
        for _ in 0..2 {
            assert!(restored.save(&path, &cells).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), external.as_bytes());
            assert_eq!(*restored.stamp.lock().unwrap(), stamp);
            assert_eq!(restored.checkpoint().unwrap().snapshot(&cells), snapshot);
        }
        let exported = path.with_file_name("rescued.ipynb");
        restored.export(&exported, &cells, None).unwrap();
        assert_eq!(*restored.stamp.lock().unwrap(), stamp);
        assert_eq!(std::fs::read(&path).unwrap(), external.as_bytes());
        assert!(restored.save(&path, &cells).is_err());
    }

    #[test]
    fn recovery_does_not_need_the_source_or_grant_permission_to_recreate_it() {
        let path = notebook_path("ipynb-recovery-missing-source");
        let (file, _) = File::read(&path).unwrap();
        let snapshot = file
            .checkpoint()
            .unwrap()
            .snapshot(&[(0, true, "recovered".into())]);
        let stamp = file.stamp.lock().unwrap().clone();
        std::fs::remove_file(&path).unwrap();
        let (restored, shown) = File::restore(&snapshot).unwrap();
        let cells = shown_cells(&shown);
        assert_eq!(shown["cells"][0]["value"], "recovered");
        assert_eq!(*restored.stamp.lock().unwrap(), stamp);
        assert!(restored.save(&path, &cells).is_err());
        assert!(!path.exists());

        let exported = path.with_file_name("rescued.ipynb");
        restored.export(&exported, &cells, None).unwrap();
        assert!(!path.exists());
        assert_eq!(*restored.stamp.lock().unwrap(), stamp);
    }

    #[test]
    fn recovery_without_a_stamp_stays_create_only() {
        let path = notebook_path("ipynb-recovery-no-stamp");
        let original = std::fs::read(&path).unwrap();
        let (file, _) = File::decode(&original).unwrap();
        let snapshot = file
            .checkpoint()
            .unwrap()
            .snapshot(&[(0, true, "recovered".into())]);
        assert_eq!(snapshot["stamp"], Value::Null);
        let (restored, shown) = File::restore(&snapshot).unwrap();
        let cells = shown_cells(&shown);
        assert!(restored.save(&path, &cells).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(restored.stamp.lock().unwrap().is_none());
        let exported = path.with_file_name("rescued.ipynb");
        restored.export(&exported, &cells, None).unwrap();
        assert!(restored.stamp.lock().unwrap().is_none());
        assert!(restored.save(&exported, &cells).is_err());
        restored
            .save(&path.with_file_name("created.ipynb"), &cells)
            .unwrap();
        assert!(restored.stamp.lock().unwrap().is_some());
    }

    #[test]
    fn incomplete_recovery_hashes_do_not_authorize_overwriting_a_live_source() {
        let path = notebook_path("ipynb-weak-recovery-stamp");
        let original = std::fs::read(&path).unwrap();
        let (file, _) = File::read(&path).unwrap();
        let snapshot = file
            .checkpoint()
            .unwrap()
            .snapshot(&[(0, true, "recovered".into())]);
        for hash in [Value::Null, json!("incomplete"), json!("z".repeat(64))] {
            let mut snapshot = snapshot.clone();
            snapshot["stamp"]["hash"] = hash;
            let (restored, shown) = File::restore(&snapshot).unwrap();
            assert!(restored.save(&path, &shown_cells(&shown)).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), original);
        }
    }

    #[test]
    fn exports_check_the_destination_without_advancing_the_source_revision() {
        let path = notebook_path("ipynb-export-cas");
        let original = std::fs::read(&path).unwrap();
        let (file, _) = File::read(&path).unwrap();
        let stamp = file.stamp.lock().unwrap().clone();
        let exported = path.with_file_name("existing.ipynb");
        std::fs::write(&exported, b"destination").unwrap();
        let expected = crate::remote::revision(&exported).unwrap();
        let cells = vec![(0, true, "copy".into())];
        for revision in [None, stamp.as_ref()] {
            assert!(file.export(&exported, &cells, revision).is_err());
            assert_eq!(std::fs::read(&exported).unwrap(), b"destination");
            assert_eq!(*file.stamp.lock().unwrap(), stamp);
        }
        file.export(&exported, &cells, expected.as_ref()).unwrap();
        let bytes = std::fs::read(&exported).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap(),
            file.content(&cells)
        );
        assert_eq!(*file.stamp.lock().unwrap(), stamp);
        assert!(
            file.export(&exported, &[(0, true, "stale".into())], expected.as_ref())
                .is_err()
        );
        assert_eq!(std::fs::read(&exported).unwrap(), bytes);
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(*file.stamp.lock().unwrap(), stamp);
        file.save(&path, &[(0, true, "saved to source".into())])
            .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn failed_export_retains_both_revisions_for_a_retry() {
        let path = notebook_path("ipynb-export-failed");
        let original = std::fs::read(&path).unwrap();
        let (file, _) = File::read(&path).unwrap();
        let stamp = file.stamp.lock().unwrap().clone();
        let exported = path.with_file_name("existing.ipynb");
        let linked = path.with_file_name("linked.ipynb");
        std::fs::write(&exported, b"destination").unwrap();
        std::fs::hard_link(&exported, &linked).unwrap();
        let expected = crate::remote::revision(&exported).unwrap();
        let cells = vec![(0, true, "copy".into())];
        assert!(file.export(&exported, &cells, expected.as_ref()).is_err());
        assert_eq!(std::fs::read(&exported).unwrap(), b"destination");
        assert_eq!(std::fs::read(&linked).unwrap(), b"destination");
        assert_eq!(crate::remote::revision(&exported).unwrap(), expected);
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(*file.stamp.lock().unwrap(), stamp);
        std::fs::remove_file(&linked).unwrap();
        file.export(&exported, &cells, expected.as_ref()).unwrap();
        assert_eq!(*file.stamp.lock().unwrap(), stamp);
    }

    #[test]
    fn invalid_or_unsupported_recovery_copies_are_refused() {
        let (file, _) = File::decode(&serde_json::to_vec(&rich_notebook()).unwrap()).unwrap();
        let snapshot = file
            .checkpoint()
            .unwrap()
            .snapshot(&[(1, true, "recovered".into())]);
        for invalid in [
            Value::Null,
            json!([]),
            json!({}),
            json!({"version":"1","content":snapshot["content"],"stamp":null}),
            json!({"version":1,"stamp":null}),
            json!({"version":1,"content":snapshot["content"]}),
            json!({"version":1,"content":snapshot["content"],"stamp":{"length":"wrong","mtime":0}}),
            json!({"version":1,"content":snapshot["content"],"stamp":false}),
            json!({"version":1,"content":snapshot["content"],"stamp":{}}),
            json!({"version":1,"content":null,"stamp":null}),
            json!({"version":1,"content":{"nbformat":3,"cells":[]},"stamp":null}),
            json!({"version":1,"content":{"nbformat":4},"stamp":null}),
            json!({"version":1,"content":{"nbformat":4,"cells":[{"source":"x"}]},"stamp":null}),
            json!({"version":1,"content":{"nbformat":4,"cells":[{"cell_type":"code","source":[42]}]},"stamp":null}),
        ] {
            assert!(File::restore(&invalid).is_err(), "accepted {invalid}");
        }
        for version in [0, 2, u64::MAX] {
            let mut unsupported = snapshot.clone();
            unsupported["version"] = version.into();
            assert_eq!(
                File::restore(&unsupported).err().unwrap(),
                "This notebook recovery version is not supported"
            );
        }
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
