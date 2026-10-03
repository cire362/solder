// The example plugin in TypeScript: the word count of the file in front,
// in the status bar, and a command that writes it at the cursor.
//
//     npx tsc -p plugins/word-count-ts
//
// then copy `plugin.json` and the `plugin.js` it writes into a folder under
// Solder's `plugins` folder.

function words(text: string): number {
  // A split on a plain string: the regular expression engine is slow here.
  let count = 0;
  for (const line of text.split("\n")) {
    for (const part of line.split(" ")) {
      if (part.trim() !== "") count++;
    }
  }
  return count;
}

function label(count: number): string {
  return count === 1 ? "1 word" : `${count} words`;
}

function show(): void {
  solder.status(label(words(solder.editor().text)));
}

solder.on("open", show);
solder.on("change", show);
solder.on("save", show);

solder.command("insert", () => {
  const file = solder.editor();
  if (file.path === null) return;
  solder.edit(file.path, file.selectionStart, file.selectionEnd, label(words(file.text)));
});
