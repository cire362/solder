'use strict';
// Debugging an extension sets up in code: what the adapter for a kind of
// program is, and what it is asked to run.
//
// The editor starts adapters and talks to them itself. So an extension is
// asked two things when a program of its kind is to be debugged: what the
// launch is, once its providers have gone over it, and where the adapter
// is. An adapter that is an object in the extension's own code is given a
// port of this machine to be reached at, so that to the editor it is an
// adapter like any other.

const net = require('net');
const path = require('path');
const { classes } = require('./types');

const { Disposable, EventEmitter, Uri, CancellationTokenSource } = classes;

class DebugAdapterExecutable {
  constructor(command, args = [], options) {
    this.command = command;
    this.args = args;
    this.options = options;
  }
}
class DebugAdapterServer {
  constructor(port, host) {
    this.port = port;
    this.host = host;
  }
}
class DebugAdapterNamedPipeServer {
  constructor(pipe) {
    this.path = pipe;
  }
}
class DebugAdapterInlineImplementation {
  constructor(implementation) {
    this.implementation = implementation;
  }
}
class Breakpoint {
  constructor(enabled = true, condition, hitCondition, logMessage) {
    Object.assign(this, { enabled, condition, hitCondition, logMessage });
  }
}
class SourceBreakpoint extends Breakpoint {
  constructor(location, ...rest) {
    super(...rest);
    this.location = location;
  }
}
class FunctionBreakpoint extends Breakpoint {
  constructor(functionName, ...rest) {
    super(...rest);
    this.functionName = functionName;
  }
}

const debugClasses = {
  DebugAdapterExecutable,
  DebugAdapterServer,
  DebugAdapterNamedPipeServer,
  DebugAdapterInlineImplementation,
  Breakpoint,
  SourceBreakpoint,
  FunctionBreakpoint,
};
const debugEnums = {
  DebugConfigurationProviderTriggerKind: { Initial: 1, Dynamic: 2 },
  DebugConsoleMode: { Separate: 0, MergeWithParent: 1 },
};

// An adapter that is an object of the extension's, reached at a port: what
// arrives there is given to it a message at a time, and what it sends goes
// back the same way, in the framing every adapter speaks.
function bridge(implementation, say) {
  return new Promise((resolve, reject) => {
    const server = net.createServer((socket) => {
      // One program is debugged through it, and then it is done.
      server.close();
      let pending = Buffer.alloc(0);
      const sending = implementation.onDidSendMessage((message) => {
        const body = Buffer.from(JSON.stringify(message));
        socket.write(`Content-Length: ${body.length}\r\n\r\n`);
        socket.write(body);
      });
      socket.on('data', (chunk) => {
        pending = Buffer.concat([pending, chunk]);
        for (;;) {
          const head = pending.indexOf('\r\n\r\n');
          if (head < 0) return;
          const length = /Content-Length: *(\d+)/i.exec(pending.slice(0, head).toString());
          const start = head + 4;
          const size = length ? Number(length[1]) : 0;
          if (pending.length < start + size) return;
          const body = pending.slice(start, start + size).toString();
          pending = pending.slice(start + size);
          try {
            implementation.handleMessage(JSON.parse(body));
          } catch (error) {
            say('error', [error && error.stack || error]);
          }
        }
      });
      const done = () => {
        if (sending && sending.dispose) sending.dispose();
        if (implementation.dispose) implementation.dispose();
      };
      socket.on('close', done);
      socket.on('error', () => socket.destroy());
    });
    server.on('error', reject);
    server.listen(0, '127.0.0.1', () => resolve(server.address().port));
  });
}

module.exports = function build(core) {
  const factories = new Map();
  const providers = [];
  const started = new EventEmitter();
  const ended = new EventEmitter();
  const activeChanged = new EventEmitter();
  const never = new EventEmitter();
  const token = new CancellationTokenSource().token;
  // The sessions the editor said are running, by its numbers.
  const sessions = new Map();
  let active;

  function folderOf(folder) {
    return folder ? { uri: Uri.file(folder), name: path.basename(folder), index: 0 } : undefined;
  }
  function session(id, type, name, configuration, folder) {
    return {
      id: String(id),
      type,
      name: name || type,
      configuration,
      workspaceFolder: folderOf(folder),
      parentSession: undefined,
      customRequest: () => Promise.reject(new Error('Requests to the adapter of a running session are not here yet')),
      getDebugProtocolBreakpoint: async () => undefined,
    };
  }

  // What the manifest says the adapter is, which a factory may hand back
  // as it is or with something added.
  function declared(type) {
    const all = (core.manifest.contributes && core.manifest.contributes.debuggers) || [];
    const one = all.find((entry) => entry.type === type);
    if (!one || !one.program) return undefined;
    const program = path.resolve(core.extensionDir, one.program);
    return one.runtime
      ? new DebugAdapterExecutable(one.runtime, [...(one.runtimeArgs || []), program, ...(one.args || [])])
      : new DebugAdapterExecutable(program, one.args || []);
  }

  const debug = core.namespace('debug', {
    registerDebugAdapterDescriptorFactory(type, factory) {
      factories.set(type, factory);
      return new Disposable(() => {
        if (factories.get(type) === factory) factories.delete(type);
      });
    },
    registerDebugConfigurationProvider(type, provider) {
      const entry = { type, provider };
      providers.push(entry);
      return new Disposable(() => {
        const at = providers.indexOf(entry);
        if (at >= 0) providers.splice(at, 1);
      });
    },
    // A launch by its name is one of a file of launches, which Solder has
    // none of; one given whole is started.
    async startDebugging(folder, nameOrConfiguration) {
      if (!nameOrConfiguration || typeof nameOrConfiguration !== 'object') return false;
      const from = folder && folder.uri ? folder.uri.fsPath : undefined;
      return (await core.request('debug.start', { configuration: core.plain(nameOrConfiguration), folder: from })) === true;
    },
    stopDebugging: () => core.request('debug.stop', {}).then(() => undefined),
    get activeDebugSession() {
      return active;
    },
    activeDebugConsole: {
      append: (text) => core.say('info', [String(text)]),
      appendLine: (text) => core.say('info', [String(text)]),
    },
    breakpoints: [],
    onDidStartDebugSession: started.event,
    onDidTerminateDebugSession: ended.event,
    onDidChangeActiveDebugSession: activeChanged.event,
    onDidReceiveDebugSessionCustomEvent: never.event,
    onDidChangeBreakpoints: never.event,
  });

  const asked = {
    // What is to be debugged and with what: the launch after the
    // extension's providers went over it, and where its adapter is.
    'debug.adapter': async ({ type, configuration, folder }) => {
      const where = folderOf(folder);
      let launch = configuration;
      const mine = providers.filter((entry) => entry.type === type || entry.type === '*');
      for (const step of ['resolveDebugConfiguration', 'resolveDebugConfigurationWithSubstitutedVariables']) {
        for (const { provider } of mine) {
          if (!provider[step]) continue;
          launch = await provider[step](where, launch, token);
          // A provider that gives nothing called the launch off.
          if (launch === undefined || launch === null) return { cancelled: true };
        }
      }
      const factory = factories.get(type) || factories.get('*');
      if (!factory) return { configuration: launch };
      const about = session(0, type, launch.name, launch, folder);
      const descriptor = await factory.createDebugAdapterDescriptor(about, declared(type));
      if (!descriptor) return { configuration: launch };
      if (descriptor instanceof DebugAdapterInlineImplementation) {
        return { configuration: launch, port: await bridge(descriptor.implementation, core.say) };
      }
      if (descriptor instanceof DebugAdapterServer) {
        return { configuration: launch, port: descriptor.port, host: descriptor.host };
      }
      if (descriptor instanceof DebugAdapterNamedPipeServer) {
        throw new Error('An adapter on a named pipe is not here yet');
      }
      const options = descriptor.options || {};
      return {
        configuration: launch,
        command: descriptor.command,
        args: descriptor.args || [],
        env: options.env,
        cwd: options.cwd,
      };
    },
  };

  const told = {
    'debug.started'({ id, type, name, configuration, folder }) {
      const one = session(id, type, name, configuration, folder);
      sessions.set(id, one);
      active = one;
      started.fire(one);
      activeChanged.fire(one);
    },
    'debug.ended'({ id }) {
      const one = sessions.get(id);
      if (!one) return;
      sessions.delete(id);
      ended.fire(one);
      if (active === one) {
        active = [...sessions.values()].pop();
        activeChanged.fire(active);
      }
    },
  };

  return { debug, asked, told, classes: debugClasses, enums: debugEnums };
};
