const vscode = require('vscode');
let failNext = false;
const reader = {
 deserializeNotebook(bytes) {
  const raw = JSON.parse(Buffer.from(bytes).toString());
  const data = new vscode.NotebookData(raw.cells.map(raw => {
   const cell = new vscode.NotebookCellData(raw.kind, raw.value, raw.languageId);
   cell.metadata = raw.metadata;
   cell.executionSummary = raw.executionSummary;
   cell.outputs = (raw.outputs || []).map(output => new vscode.NotebookCellOutput(output.items.map(item => new vscode.NotebookCellOutputItem(Buffer.from(item.data, 'base64'), item.mime)), output.metadata));
   return cell;
  }));
  data.metadata = raw.metadata;
  return data;
 },
 serializeNotebook(data) {
  if (failNext) { failNext = false; throw new Error('Roadmap fixture refuses this save'); }
  const raw = {metadata: data.metadata, cells: data.cells.map(cell => ({kind: cell.kind, value: cell.value, languageId: cell.languageId, metadata:cell.metadata, executionSummary: cell.executionSummary, outputs: (cell.outputs || []).map(output => ({metadata:output.metadata, items:output.items.map(item => ({mime:item.mime,data:Buffer.from(item.data).toString('base64')}))}))}))};
  return Buffer.from(JSON.stringify(raw, null, 2));
 }
};
exports.activate = context => {
 let count = 0;
 const controller = vscode.notebooks.createNotebookController('roadmap.runner', 'roadmap-book', 'Roadmap fixture runner', async (cells, notebook, controller) => {
  for (const cell of cells) {
   const run = controller.createNotebookCellExecution(cell);
   run.executionOrder = ++count;
   run.start();
   await run.replaceOutput(new vscode.NotebookCellOutput([vscode.NotebookCellOutputItem.text('fixture output: ' + cell.document.getText())], {keep:'execution-output'}));
   run.end(true);
  }
 });
 context.subscriptions.push(controller, vscode.workspace.registerNotebookSerializer('roadmap-book',reader), vscode.commands.registerCommand('roadmap.failSave',()=>{failNext=true;}), vscode.commands.registerCommand('roadmap.saveBook',()=>vscode.workspace.notebookDocuments[0].save()));
};
