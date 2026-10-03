// The script the host's tests run: the JavaScript twin of `probe`. A
// command's id says what to do, with an argument after a colon; the outcome
// goes to the status bar.
let events = 0;

// Work the editor can time.
function spin(rounds) {
  let x = 0;
  for (let i = 0; i < rounds; i++) x = (x * 31 + i) | 0;
  return x;
}

function show(run, ok) {
  let text;
  try {
    text = ok(run());
  } catch (error) {
    text = "refused: " + error.message;
  }
  solder.status(text);
}

solder.on("activate", () => { events++; solder.status("active"); });
solder.on("open", (event) => { events++; solder.status("open " + event.path); });
solder.on("save", (event) => { events++; solder.status("save " + event.path); });
// Typing: enough work to go over a small budget.
solder.on("change", (event) => { events++; spin(2000); solder.status("change " + event.path); });

const commands = {
  count: () => solder.status(events + " events"),
  editor: () => show(() => solder.editor(), (e) => `${e.path} ${e.selectionStart}..${e.selectionEnd} ${e.text.length}`),
  // Replaces the selection, by the positions JavaScript counts in.
  wrap: () => {
    const e = solder.editor();
    const picked = e.text.slice(e.selectionStart, e.selectionEnd);
    show(() => solder.edit(e.path, e.selectionStart, e.selectionEnd, `[${picked}]`), () => "edited " + picked);
  },
  blind: () => show(() => solder.edit("src/a.rs", 0, 1, "x"), () => "edited"),
  read: (arg) => show(() => solder.readFile(arg), (text) => text),
  get: (arg) => show(() => solder.http({ url: arg, headers: { "x-probe": "1" } }), (r) => `${r.status} ${r.body}`),
  forever: () => { for (;;) spin(1000); },
  throw: () => { throw new Error("probe asked to throw"); },
  log: (arg) => console.log(arg, 1 + 1),
  later: async () => { await Promise.resolve(); const n = await Promise.resolve(41); solder.status("later " + (n + 1)); },
  escape: () => solder.status([typeof std, typeof os, typeof require, typeof solder.on].join(" ")),
};

for (const id of [
  "count", "editor", "wrap", "blind", "read:notes.txt", "read:missing.txt",
  "get:http://127.0.0.1:8080/x", "get:http://example.com/x",
  "forever", "throw", "log:hello", "later", "escape",
]) {
  const [name, ...rest] = id.split(":");
  solder.command(id, () => { events++; return commands[name](rest.join(":")); });
}
