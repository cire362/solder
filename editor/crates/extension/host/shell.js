'use strict';
// What an extension runs and watches: terminals, tasks, and files that
// change on disk.
//
// A terminal is the editor's own, in its dock: the extension names what
// runs in it and types into it. A task is a command with a name, run in
// such a terminal. Watching files needs nothing of the editor: the code of
// an extension reaches the disk itself, and Node says when it changes.

const fs = require('fs');
const path = require('path');
const { classes, glob } = require('./types');

const { Disposable, EventEmitter, Uri, CancellationTokenSource } = classes;

// A command line for a shell, or a command with what it is given.
class ShellExecution {
  constructor(command, argsOrOptions, options) {
    if (Array.isArray(argsOrOptions)) {
      this.command = command;
      this.args = argsOrOptions;
      this.options = options;
    } else {
      this.commandLine = command;
      this.options = argsOrOptions;
    }
  }
}
class ProcessExecution {
  constructor(process, argsOrOptions, options) {
    this.process = process;
    this.args = Array.isArray(argsOrOptions) ? argsOrOptions : [];
    this.options = Array.isArray(argsOrOptions) ? options : argsOrOptions;
  }
}
class CustomExecution {
  constructor(callback) {
    this.callback = callback;
  }
}
class TaskGroup {
  constructor(id, label) {
    this.id = id;
    this.label = label;
  }
}
TaskGroup.Clean = new TaskGroup('clean', 'Clean');
TaskGroup.Build = new TaskGroup('build', 'Build');
TaskGroup.Rebuild = new TaskGroup('rebuild', 'Rebuild');
TaskGroup.Test = new TaskGroup('test', 'Test');

class Task {
  // (definition, scope, name, source, execution, matchers), or the older
  // one without the scope.
  constructor(definition, a, b, c, d, e) {
    const [scope, name, source, execution, matchers] = typeof a === 'string' ? [undefined, a, b, c, d] : [a, b, c, d, e];
    this.definition = definition;
    this.scope = scope;
    this.name = name;
    this.source = source;
    this.execution = execution;
    this.problemMatchers = [].concat(matchers || []);
    this.isBackground = false;
    this.presentationOptions = {};
    this.runOptions = {};
    this.group = undefined;
    this.detail = undefined;
  }
}

const shellClasses = { ShellExecution, ProcessExecution, CustomExecution, TaskGroup, Task };
const shellEnums = {
  TaskScope: { Global: 1, Workspace: 2 },
  TaskRevealKind: { Always: 1, Silent: 2, Never: 3 },
  TaskPanelKind: { Shared: 1, Dedicated: 2, New: 3 },
  ShellQuoting: { Escape: 1, Strong: 2, Weak: 3 },
  TerminalLocation: { Panel: 1, Editor: 2 },
  TerminalExitReason: { Unknown: 0, Shutdown: 1, Process: 2, User: 3, Extension: 4 },
};

// One word of a command line, quoted where a shell would split it.
function quoted(word) {
  const text = typeof word === 'string' ? word : word.value;
  return /^[\w@%+=:,./-]+$/.test(text) ? text : `'${text.replace(/'/g, `'\\''`)}'`;
}

module.exports = function build(core) {
  const { notify } = core;
  const never = new EventEmitter();
  const token = new CancellationTokenSource().token;

  // ------------------------------------------------------------- terminals

  const terminals = new Map();
  const opened = new EventEmitter();
  const closed = new EventEmitter();
  const activeChanged = new EventEmitter();
  let nextTerminal = 1;
  let active;

  // A terminal is made in the editor when it is first shown or typed
  // into: one an extension only keeps ready takes no room in the dock.
  function createTerminal(nameOrOptions, shellPath, shellArgs) {
    const options = nameOrOptions && typeof nameOrOptions === 'object'
      ? nameOrOptions
      : { name: nameOrOptions, shellPath, shellArgs };
    const id = nextTerminal++;
    let made = false;
    const cwd = options.cwd && typeof options.cwd === 'object' ? options.cwd.fsPath : options.cwd;
    const args = typeof options.shellArgs === 'string' ? [options.shellArgs] : options.shellArgs;
    const make = (show) => {
      if (made || terminal.exitStatus) return;
      made = true;
      notify('terminal.create', {
        id,
        name: options.name,
        program: options.shellPath,
        args: args || [],
        cwd,
        env: options.env,
        keep: !!options._task,
        show,
      });
    };
    const terminal = {
      name: options.name || 'Terminal',
      creationOptions: options,
      exitStatus: undefined,
      state: { isInteractedWith: false },
      processId: Promise.resolve(undefined),
      shellIntegration: undefined,
      sendText(text, newline = true) {
        make(false);
        notify('terminal.send', { id, text: String(text) + (newline ? '\n' : '') });
      },
      show() {
        if (made) notify('terminal.show', { id });
        else make(true);
        if (active !== terminal) {
          active = terminal;
          activeChanged.fire(terminal);
        }
      },
      hide() {},
      dispose() {
        if (made) notify('terminal.dispose', { id });
        else end(id, undefined);
      },
    };
    // One the extension draws itself has no place in the dock yet.
    if (options.pty) core.missing('window.createTerminal (pty)');
    terminals.set(id, terminal);
    opened.fire(terminal);
    return terminal;
  }
  function end(id, code) {
    const terminal = terminals.get(id);
    if (!terminal) return;
    terminals.delete(id);
    terminal.exitStatus = { code, reason: code === undefined ? 4 : 2 };
    if (active === terminal) {
      active = undefined;
      activeChanged.fire(undefined);
    }
    closed.fire(terminal);
  }

  const windowMembers = {
    createTerminal,
    get terminals() {
      return [...terminals.values()];
    },
    get activeTerminal() {
      return active;
    },
    onDidOpenTerminal: opened.event,
    onDidCloseTerminal: closed.event,
    onDidChangeActiveTerminal: activeChanged.event,
    onDidChangeTerminalState: never.event,
  };

  // ----------------------------------------------------------------- tasks

  const providers = [];
  const running = new Map();
  const taskStarted = new EventEmitter();
  const taskEnded = new EventEmitter();
  const processStarted = new EventEmitter();
  const processEnded = new EventEmitter();
  // What `tasks.fetch` last gave the editor, which runs one by its number.
  let listed = [];

  async function fetchTasks(filter) {
    const all = [];
    for (const { type, provider } of providers) {
      if (filter && filter.type && filter.type !== type) continue;
      try {
        all.push(...((await provider.provideTasks(token)) || []));
      } catch (error) {
        core.say('error', [error && error.stack || error]);
      }
    }
    return all;
  }

  // What a task runs, as a program with what it is given.
  function command(execution) {
    const options = execution.options || {};
    if (execution instanceof ProcessExecution) {
      return { program: execution.process, args: execution.args.map(String), options };
    }
    const line = execution.commandLine !== undefined
      ? execution.commandLine
      : [execution.command, ...(execution.args || [])].map(quoted).join(' ');
    const shell = options.executable || process.env.SHELL || '/bin/sh';
    return { program: shell, args: [...(options.shellArgs || ['-c']), line], options };
  }

  async function executeTask(task) {
    let resolved = task;
    // A task a provider only named is filled in when it is run.
    if (!resolved.execution) {
      const owner = providers.find((entry) => entry.type === (task.definition && task.definition.type));
      if (owner && owner.provider.resolveTask) resolved = (await owner.provider.resolveTask(task, token)) || task;
    }
    const execution = resolved.execution;
    if (!execution || execution instanceof CustomExecution) {
      core.missing('tasks (CustomExecution)');
      throw new Error(`The task ${task.name} runs in the extension's own code, which is not here yet`);
    }
    const { program, args, options } = command(execution);
    const terminal = createTerminal({
      name: resolved.name,
      shellPath: program,
      shellArgs: args,
      cwd: options.cwd,
      env: options.env,
      _task: true,
    });
    const run = { task: resolved, terminate: () => terminal.dispose() };
    running.set(terminal, run);
    terminal.show();
    taskStarted.fire({ execution: run });
    processStarted.fire({ execution: run, processId: undefined });
    return run;
  }
  // A task ends when what ran in its terminal does.
  closed.event((terminal) => {
    const run = running.get(terminal);
    if (!run) return;
    running.delete(terminal);
    processEnded.fire({ execution: run, exitCode: terminal.exitStatus && terminal.exitStatus.code });
    taskEnded.fire({ execution: run });
  });

  const tasks = core.namespace('tasks', {
    registerTaskProvider(type, provider) {
      const entry = { type, provider };
      providers.push(entry);
      return new Disposable(() => {
        const at = providers.indexOf(entry);
        if (at >= 0) providers.splice(at, 1);
      });
    },
    fetchTasks,
    executeTask,
    get taskExecutions() {
      return [...running.values()];
    },
    onDidStartTask: taskStarted.event,
    onDidEndTask: taskEnded.event,
    onDidStartTaskProcess: processStarted.event,
    onDidEndTaskProcess: processEnded.event,
  });

  // ----------------------------------------------------------- file watching

  // Every folder under `root`, each watched on its own: watching a whole
  // tree at once is not something every system Node runs on can do.
  function watchTree(root, changed) {
    const watching = new Map();
    const add = (dir) => {
      if (watching.has(dir)) return;
      let watcher;
      try {
        watcher = fs.watch(dir, (event, name) => {
          if (name) changed(path.join(dir, String(name)));
        });
      } catch {
        return;
      }
      watcher.on('error', () => drop(dir));
      watching.set(dir, watcher);
      let entries = [];
      try {
        entries = fs.readdirSync(dir, { withFileTypes: true });
      } catch {
        // Gone again already.
      }
      for (const entry of entries) {
        if (entry.isDirectory() && entry.name !== '.git' && entry.name !== 'node_modules') add(path.join(dir, entry.name));
      }
    };
    const drop = (dir) => {
      for (const [known, watcher] of watching) {
        if (known === dir || known.startsWith(dir + path.sep)) {
          watcher.close();
          watching.delete(known);
        }
      }
    };
    add(root);
    return { add, drop, close: () => drop(root) };
  }

  function createFileSystemWatcher(pattern, ignoreCreate, ignoreChange, ignoreDelete) {
    const created = new EventEmitter();
    const changed = new EventEmitter();
    const deleted = new EventEmitter();
    const roots = typeof pattern === 'string' ? core.state.folders : [pattern.base];
    const wanted = glob(typeof pattern === 'string' ? pattern : pattern.pattern);
    // What is known to be there, to tell a new file from a changed one.
    const known = new Set();
    const pending = new Map();
    const trees = roots.map((root) => {
      const tree = watchTree(root, (file) => {
        // Several signs of one change come at once; they are one change.
        clearTimeout(pending.get(file));
        pending.set(file, setTimeout(() => {
          pending.delete(file);
          let stat;
          try {
            stat = fs.statSync(file);
          } catch {
            stat = undefined;
          }
          if (stat && stat.isDirectory()) {
            tree.add(file);
            // What was put in it before it was watched is new too.
            const inside = (dir) => {
              let entries = [];
              try {
                entries = fs.readdirSync(dir, { withFileTypes: true });
              } catch {
                return;
              }
              for (const entry of entries) {
                const full = path.join(dir, entry.name);
                if (entry.isDirectory()) inside(full);
                else if (!known.has(full)) {
                  known.add(full);
                  const within = path.relative(root, full).split(path.sep).join('/');
                  if (wanted.test(within) && !ignoreCreate) created.fire(Uri.file(full));
                }
              }
            };
            inside(file);
            return;
          }
          if (!stat) tree.drop(file);
          const relative = path.relative(root, file).split(path.sep).join('/');
          if (!wanted.test(relative)) return;
          const uri = Uri.file(file);
          if (!stat) {
            if (known.delete(file) && !ignoreDelete) deleted.fire(uri);
          } else if (known.has(file)) {
            if (!ignoreChange) changed.fire(uri);
          } else {
            known.add(file);
            if (!ignoreCreate) created.fire(uri);
          }
        }, 30));
      });
      return tree;
    });
    // The files there already are not new when they change.
    const seed = (dir) => {
      let entries = [];
      try {
        entries = fs.readdirSync(dir, { withFileTypes: true });
      } catch {
        return;
      }
      for (const entry of entries) {
        const full = path.join(dir, entry.name);
        if (entry.isDirectory()) {
          if (entry.name !== '.git' && entry.name !== 'node_modules') seed(full);
        } else {
          known.add(full);
        }
      }
    };
    roots.forEach(seed);
    return {
      ignoreCreateEvents: !!ignoreCreate,
      ignoreChangeEvents: !!ignoreChange,
      ignoreDeleteEvents: !!ignoreDelete,
      onDidCreate: created.event,
      onDidChange: changed.event,
      onDidDelete: deleted.event,
      dispose() {
        trees.forEach((tree) => tree.close());
        pending.forEach((timer) => clearTimeout(timer));
      },
    };
  }

  // What the editor asks: the tasks there are, and to run one of them.
  const asked = {
    'tasks.fetch': async () => {
      listed = await fetchTasks();
      return listed.map((task) => ({ name: String(task.name), source: task.source, detail: task.detail }));
    },
    'tasks.run': async ({ index }) => {
      const task = listed[index];
      if (!task) throw new Error('That task is not there any more');
      await executeTask(task);
      return true;
    },
  };
  const told = {
    'terminal.closed': ({ id, code }) => end(id, code === null ? undefined : code),
  };

  return { windowMembers, tasks, createFileSystemWatcher, asked, told, classes: shellClasses, enums: shellEnums };
};
