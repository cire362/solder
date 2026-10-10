'use strict';
// What an extension shows in the sidebar: trees of its own, the changes of
// a source control it knows, and the tests it found.
//
// To the editor all three are one thing: a view with a name, and nodes
// under nodes. A node has a label, may open, may run a command when
// chosen, and may have a few more things that can be done with it. So the
// editor draws one kind of list, and what each of VS Code's three ideas
// means is worked out here, where the extension's objects are.

const path = require('path');
const { classes } = require('./types');

const { Disposable, EventEmitter, Uri, CancellationTokenSource } = classes;

class TreeItem {
  constructor(labelOrUri, collapsibleState = 0) {
    if (Uri.isUri(labelOrUri)) this.resourceUri = labelOrUri;
    else this.label = labelOrUri;
    this.collapsibleState = collapsibleState;
  }
}
class TestRunRequest {
  constructor(include, exclude, profile, continuous) {
    Object.assign(this, { include, exclude, profile, continuous });
  }
}
class TestMessage {
  constructor(message) {
    this.message = message;
  }
  static diff(message, expected, actual) {
    return Object.assign(new TestMessage(message), { expectedOutput: expected, actualOutput: actual });
  }
}
class TestTag {
  constructor(id) {
    this.id = id;
  }
}

const viewClasses = { TreeItem, TestRunRequest, TestMessage, TestTag };
const viewEnums = {
  TreeItemCollapsibleState: { None: 0, Collapsed: 1, Expanded: 2 },
  TreeItemCheckboxState: { Unchecked: 0, Checked: 1 },
  TestRunProfileKind: { Run: 1, Debug: 2, Coverage: 3 },
};

function text(value) {
  if (value === undefined || value === null) return undefined;
  if (typeof value === 'string') return value;
  if (typeof value.value === 'string') return value.value;
  if (typeof value.label === 'string') return value.label;
  return String(value);
}

module.exports = function build(core) {
  const { notify } = core;
  const token = new CancellationTokenSource().token;
  const never = new EventEmitter();
  // The views there are, by name: what each is called, and how its nodes
  // are found. A node is known to the editor by a key that stays the
  // same while the node does, so what was open stays open.
  const views = new Map();

  function declared(id) {
    const groups = (core.manifest.contributes && core.manifest.contributes.views) || {};
    for (const list of Object.values(groups)) {
      const found = (list || []).find((view) => view.id === id);
      if (found) return found.name;
    }
    return undefined;
  }

  function add(id, title, kind, children) {
    const view = { id, title, kind, children, nodes: new Map(), acts: new Map(), telling: false };
    views.set(id, view);
    notify('view', { id, title, kind });
    return view;
  }
  function remove(view) {
    if (views.get(view.id) !== view) return;
    views.delete(view.id);
    view.nodes.clear();
    view.acts.clear();
    notify('view', { id: view.id, gone: true });
  }
  // Said once for all that changed in one go.
  function changed(view) {
    if (view.telling || views.get(view.id) !== view) return;
    view.telling = true;
    setTimeout(() => {
      view.telling = false;
      if (views.get(view.id) === view) notify('view.changed', { id: view.id });
    }, 30);
  }
  // Keys name the node and its action, so refreshing one branch cannot
  // make a click on another run a different callback.
  function act(view, key, run) {
    view.acts.set(key, run);
    return key;
  }
  function forgetChildren(view, parent) {
    const below = (key) => parent === null || key.startsWith(parent + '/');
    for (const key of view.nodes.keys()) if (below(key)) view.nodes.delete(key);
    for (const key of view.acts.keys()) if (below(key)) view.acts.delete(key);
  }
  function property(object, name, value, update) {
    Object.defineProperty(object, name, {
      enumerable: true, get: () => value,
      set: (now) => { value = now; update(); },
    });
  }
  const runs = (command) => () => core.vscode.commands.executeCommand(command.command, ...(command.arguments || []));

  // ------------------------------------------------------------------ trees

  function tree(id, provider, shown) {
    const selected = new EventEmitter();
    const expanded = new EventEmitter();
    const collapsed = new EventEmitter();
    const visibility = new EventEmitter();
    const view = add(id, declared(id) || id, 'tree', async (parent) => {
      const element = parent === null ? undefined : view.nodes.get(parent);
      if (parent !== null && element === undefined) return [];
      const kids = ((await provider.getChildren(element)) || []).slice(0, 10000);
      const out = [];
      const seen = new Map();
      for (const kid of kids) {
        const item = await provider.getTreeItem(kid);
        if (item.checkboxState !== undefined) core.missing('TreeItem.checkboxState');
        const label = text(item.label) || (item.resourceUri ? path.basename(item.resourceUri.path) : '');
        // Two of one name under one parent are told apart by their turn.
        const name = encodeURIComponent(String(item.id || label));
        const turn = seen.get(name) || 0;
        seen.set(name, turn + 1);
        const key = `${parent === null ? '' : parent}/${turn ? `${name}#${turn}` : name}`;
        view.nodes.set(key, kid);
        out.push({
          key,
          label,
          description: item.description === true ? undefined : text(item.description),
          tooltip: text(item.tooltip),
          opens: item.collapsibleState > 0,
          open: item.collapsibleState === 2,
          run: act(view, key + '#@choose', () => {
            treeView.selection = [kid];
            selected.fire({ selection: [kid] });
            return item.command ? runs(item.command)() : undefined;
          }),
        });
      }
      return out;
    });
    const refresh = provider.onDidChangeTreeData
      ? provider.onDidChangeTreeData(() => changed(view))
      : undefined;
    const treeView = {
      title: view.title,
      description: undefined,
      message: undefined,
      badge: undefined,
      visible: false,
      selection: [],
      onDidChangeSelection: selected.event,
      onDidChangeVisibility: visibility.event,
      onDidExpandElement: expanded.event,
      onDidCollapseElement: collapsed.event,
      onDidChangeCheckboxState: never.event,
      reveal: async () => core.missing('TreeView.reveal'),
      dispose() {
        if (refresh) refresh.dispose();
        remove(view);
      },
    };
    view.tree = treeView;
    view.expanded = expanded; view.collapsed = collapsed; view.visibility = visibility;
    for (const name of ['title', 'description', 'message', 'badge']) {
      property(treeView, name, treeView[name], () => {
        view.title = treeView.title;
        notify('view', { id, title: view.title, kind: 'tree', message: text(treeView.message) });
      });
    }
    if (shown && shown.canSelectMany) core.missing('TreeView.canSelectMany');
    return treeView;
  }

  // --------------------------------------------------------- source control

  function createSourceControl(id, label, rootUri) {
    const groups = [];
    const inputChange = new EventEmitter();
    const input = { value: '', placeholder: '', enabled: true, visible: true, onDidChange: inputChange.event };
    const control = {
      id,
      label,
      rootUri,
      inputBox: input,
      count: undefined,
      quickDiffProvider: undefined,
      commitTemplate: undefined,
      acceptInputCommand: undefined,
      statusBarCommands: undefined,
      createResourceGroup(groupId, groupLabel) {
        let states = [];
        const group = {
          id: groupId,
          label: groupLabel,
          hideWhenEmpty: false,
          get resourceStates() {
            return states;
          },
          set resourceStates(now) {
            states = (now || []).slice(0, 10000);
            changed(view);
          },
          dispose() {
            const at = groups.indexOf(group);
            if (at >= 0) groups.splice(at, 1);
            changed(view);
          },
        };
        for (const name of ['label', 'hideWhenEmpty']) property(group, name, group[name], () => changed(view));
        groups.push(group);
        changed(view);
        return group;
      },
      dispose: () => remove(view),
    };
    const view = add(`scm:${id}`, label, 'scm', async (parent) => {
      if (parent === null) {
        const out = [];
        // The message for what is about to be recorded, and the command
        // that records it.
        if (input.visible !== false) {
          out.push({
            key: '/message',
            label: input.value || input.placeholder || 'Message',
            description: input.value ? undefined : 'not written yet',
            run: input.enabled === false ? undefined : act(view, '/message#@choose', async () => {
              const typed = await core.vscode.window.showInputBox({ prompt: input.placeholder || 'Message', value: input.value });
              if (typed !== undefined) {
                input.value = typed;
                changed(view);
              }
            }),
          });
        }
        if (control.acceptInputCommand) {
          const command = control.acceptInputCommand;
          out.push({ key: '/accept', label: command.title || 'Commit', run: act(view, '/accept#@choose', runs(command)) });
        }
        for (const group of groups) {
          if (group.hideWhenEmpty && !group.resourceStates.length) continue;
          out.push({
            key: `/group:${encodeURIComponent(group.id)}`,
            label: group.label,
            description: String(group.resourceStates.length),
            opens: true,
            open: true,
          });
        }
        return out;
      }
      const group = groups.find((one) => parent === `/group:${encodeURIComponent(one.id)}`);
      if (!group) return [];
      return group.resourceStates.map((state) => {
        const file = state.resourceUri.fsPath;
        const root = rootUri ? rootUri.fsPath : '';
        const within = root && file.startsWith(root + '/') ? path.dirname(file.slice(root.length + 1)) : path.dirname(file);
        const decorations = state.decorations || {};
        return {
          key: `${parent}/${encodeURIComponent(file)}`,
          label: path.basename(file),
          description: within === '.' ? undefined : within,
          tooltip: text(decorations.tooltip),
          strike: !!decorations.strikeThrough,
          run: act(view, `${parent}/${encodeURIComponent(file)}#@choose`, state.command
            ? runs(state.command)
            : () => core.request('show', { uri: state.resourceUri.toString() })),
        };
      });
    });
    for (const name of ['value', 'placeholder', 'enabled', 'visible']) {
      property(input, name, input[name], () => {
        if (name === 'value') inputChange.fire(input.value);
        changed(view);
      });
    }
    property(control, 'acceptInputCommand', control.acceptInputCommand, () => changed(view));
    property(control, 'quickDiffProvider', undefined, () => { if (control.quickDiffProvider) core.missing('SourceControl.quickDiffProvider'); });
    return control;
  }

  const scm = core.namespace('scm', {
    createSourceControl,
    inputBox: { value: '', placeholder: '' },
  });

  // ------------------------------------------------------------------ tests

  // A list of test items by their names, as a controller and each item has.
  function collection(owner, onChange) {
    const items = new Map();
    return {
      get size() {
        return items.size;
      },
      add(item) {
        item.parent = owner;
        items.set(item.id, item);
        onChange();
      },
      delete(id) {
        const item = items.get(id);
        if (items.delete(id)) { item.parent = undefined; onChange(); }
      },
      get: (id) => items.get(id),
      replace(all) {
        for (const item of items.values()) item.parent = undefined;
        items.clear();
        for (const item of all) {
          item.parent = owner;
          items.set(item.id, item);
        }
        onChange();
      },
      forEach(callback, thisArg) {
        for (const item of items.values()) callback.call(thisArg, item, this);
      },
      [Symbol.iterator]() {
        return [...items.entries()][Symbol.iterator]();
      },
    };
  }

  const MARKS = { queued: 'Queued', running: 'Running', passed: 'Passed', failed: 'Failed', errored: 'Error', skipped: 'Skipped' };

  function createTestController(id, label) {
    const profiles = [];
    // What each test last did, and what it said when it failed.
    let states = new WeakMap();
    const view = add(`tests:${id}`, label, 'tests', async (parent) => {
      if (parent === null && controller.resolveHandler && !controller._resolved) {
        await controller.resolveHandler(undefined);
        controller._resolved = true;
      }
      const list = parent === null ? controller.items : view.nodes.get(parent) && view.nodes.get(parent).children;
      if (!list) return [];
      const item = parent === null ? undefined : view.nodes.get(parent);
      // What is under an item may be found only when it is opened.
      if (item && item.canResolveChildren && controller.resolveHandler && !item._resolved) {
        await controller.resolveHandler(item);
        item._resolved = true;
      }
      const out = [];
      if (parent === null && list.size && profiles.length) {
        out.push({ key: '/all', label: 'Run all', run: act(view, '/all#@choose', () => run(undefined)) });
      }
      list.forEach((test) => {
        const key = `${parent === null ? '' : parent}/${encodeURIComponent(test.id)}`;
        view.nodes.set(key, test);
        const state = states.get(test) || {};
        out.push({
          key,
          label: test.label,
          description: state.message || test.description,
          mark: MARKS[state.state],
          tooltip: state.details || text(test.error),
          opens: test.children.size > 0 || !!test.canResolveChildren,
          // Chosen, a test is shown where it is written.
          run: test.uri
            ? act(view, key + '#@choose', () => core.request('show', {
              uri: test.uri.toString(),
              selection: test.range ? { anchor: test.range.start, active: test.range.start } : undefined,
            }))
            : undefined,
          actions: profiles.map((profile, i) => ({ title: profile.label,
            run: act(view, key + `#@profile:${i}`, () => run([test], profile)) })),
        });
      });
      return out;
    });
    const update = () => changed(view);
    function run(include, chosen) {
      const profile = chosen || profiles.find((one) => one.isDefault && one.kind === 1) || profiles.find((one) => one.kind === 1) || profiles[0];
      if (!profile) return undefined;
      return profile.runHandler(new TestRunRequest(include, undefined, profile), token);
    }
    const controller = {
      id,
      label,
      items: undefined,
      resolveHandler: undefined,
      refreshHandler: undefined,
      createTestItem(itemId, itemLabel, uri) {
        const item = {
          id: itemId,
          label: itemLabel,
          uri,
          parent: undefined,
          tags: [],
          canResolveChildren: false,
          busy: false,
          range: undefined,
          error: undefined,
          description: undefined,
          sortText: undefined,
        };
        item.children = collection(item, update);
        for (const name of ['label', 'description', 'error', 'busy', 'canResolveChildren']) {
          property(item, name, item[name], update);
        }
        return item;
      },
      createRunProfile(profileLabel, kind, runHandler, isDefault = false, tag) {
        const profile = { label: profileLabel, kind, runHandler, isDefault, tag, supportsContinuousRun: false, configureHandler: undefined };
        profile.dispose = () => {
          const at = profiles.indexOf(profile);
          if (at >= 0) profiles.splice(at, 1);
          update();
        };
        profiles.push(profile);
        update();
        return profile;
      },
      // What a run says of each test is what the test's node shows.
      createTestRun(request, name) {
        const set = (state) => (item, message) => {
          const said = [].concat(message || [])[0];
          const first = said ? String(text(said.message) || '').split('\n')[0] : undefined;
          states.set(item, { state, message: state === 'failed' || state === 'errored' ? first : undefined,
            details: [].concat(message || []).map((one) => text(one.message)).join('\n') });
          update();
        };
        return {
          name,
          token,
          isPersisted: true,
          enqueued: set('queued'),
          started: set('running'),
          passed: set('passed'),
          failed: set('failed'),
          errored: set('errored'),
          skipped: set('skipped'),
          appendOutput: (output) => core.say('info', [String(output).replace(/\r?\n$/, '')]),
          addCoverage() { core.missing('TestRun.addCoverage'); },
          end: update,
        };
      },
      invalidateTestResults() {
        states = new WeakMap();
        update();
      },
      createTestTag: (tagId) => new TestTag(tagId),
      dispose: () => remove(view),
    };
    controller.items = collection(undefined, update);
    view.refresh = async () => {
      if (controller.refreshHandler) await controller.refreshHandler(token);
      else controller._resolved = false;
    };
    return controller;
  }

  const tests = core.namespace('tests', { createTestController });

  // ---------------------------------------------------- what the editor asks

  const asked = {
    'view.refresh': async ({ id }) => {
      const view = views.get(id);
      if (view && view.refresh) await view.refresh();
      return true;
    },
    'view.children': async ({ id, node }) => {
      const view = views.get(id);
      if (!view) return [];
      const parent = node === undefined ? null : node;
      // Drop closures for nodes no longer present, without invalidating
      // actions of a sibling whose children have not changed.
      const fetch = async () => {
        forgetChildren(view, parent);
        return view.children(parent);
      };
      view.fetching = (view.fetching || Promise.resolve()).catch(() => {}).then(fetch);
      return view.fetching;
    },
    'view.visible': async ({ id, visible }) => {
      const view = views.get(id);
      if (view && view.tree && view.tree.visible !== visible) {
        view.tree.visible = visible;
        view.visibility.fire({ visible });
      }
      return true;
    },
    'view.expand': async ({ id, node, open }) => {
      const view = views.get(id);
      const element = view && view.nodes.get(node);
      if (view && element !== undefined && view.expanded) {
        (open ? view.expanded : view.collapsed).fire({ element });
      }
      return true;
    },
    'view.run': async ({ id, run }) => {
      const view = views.get(id);
      const action = view && view.acts && view.acts.get(run);
      if (!action) throw new Error('That is not there any more');
      await action();
      return true;
    },
  };

  const decorators = new Map();
  let nextDecorator = 1;
  asked['files.decorate'] = async ({ uris }) => {
    const out = [];
    for (const uri of (uris || []).slice(0, 256)) {
      for (const provider of decorators.values()) {
        const decoration = await provider.provideFileDecoration(Uri.parse(uri), token);
        if (decoration) out.push({ uri, badge: decoration.badge, tooltip: decoration.tooltip });
      }
    }
    return out;
  };
  const windowMembers = {
    registerFileDecorationProvider(provider) {
      const id = nextDecorator++;
      decorators.set(id, provider);
      const changed = () => notify('files.changed', {});
      const subscription = provider.onDidChangeFileDecorations && provider.onDidChangeFileDecorations(changed);
      notify('files.provider', { id });
      return new Disposable(() => {
        decorators.delete(id);
        if (subscription) subscription.dispose();
        notify('files.provider', { id, gone: true });
      });
    },
    registerTreeDataProvider(id, provider) {
      const made = tree(id, provider);
      return new Disposable(() => made.dispose());
    },
    createTreeView: (id, options) => tree(id, options.treeDataProvider, options),
  };

  return { windowMembers, scm, tests, asked, classes: viewClasses, enums: viewEnums };
};
