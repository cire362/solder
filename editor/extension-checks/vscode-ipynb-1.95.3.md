# vscode.ipynb 1.95.3

Checked on 2026-10-10 by the **Extensions of Open VSX** workflow, on a
disposable Ubuntu runner with Node 22. Published extension code ran only
there. The archive was `vscode.ipynb-1.95.3.vsix` from
[Open VSX](https://open-vsx.org/api/vscode/ipynb/1.95.3/file/vscode.ipynb-1.95.3.vsix),
225,385 bytes according to its published metadata.

[The verification run](https://github.com/cire362/solder/actions/runs/38052216176)
used Solder commit `7941322b4ef89e6adec18d32bf71acf28f8c2357` and the
exact extension version `vscode.ipynb@1.95.3`. Its `extensions` artifact
contains the report.

The extension activated and its `jupyter-notebook` serializer read a format
4 file containing one Python code cell, `1 + 1`. The probe changed that
cell to `2 + 2` with a final newline, called the serializer to save, and read
the JSON file back from disk. Both reading and saving passed, including
the comparison with the edited source. The local regression test also
checks that a serializer returning the original source fails this probe.

The host reported these API calls as missing:

- `DocumentDropOrPasteEditKind`
- `workspace.onWillSaveNotebookDocument`
- `languages.registerDocumentPasteEditProvider`
- `languages.registerDocumentDropEditProvider`

Those features remain unimplemented. This check covers activation and a
small serializer round trip. It does not verify attachment paste, arbitrary
metadata or output bundles, a Jupyter kernel, cell execution, renderer
modules or the ipywidgets protocol. It does not establish compatibility
with the separate Jupyter extension or other versions of this reader.
