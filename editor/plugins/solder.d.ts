// What a Solder plugin written in JavaScript or TypeScript can use: the
// global `solder`. A plugin is one script, `plugin.js`, next to a
// `plugin.json` that lists the permissions it needs. There are no modules,
// no timers and no files: the script talks to the editor and nothing else.

/** Something that happened in the editor. */
type SolderEvent =
  | { event: "activate" }
  | { event: "command"; id: string }
  | { event: "open"; path: string; language: string | null }
  | { event: "save"; path: string }
  | { event: "change"; path: string };

interface SolderEditor {
  /** From the project's root; null for a file not saved yet. */
  path: string | null;
  language: string | null;
  text: string;
  /** Positions in `text`, as JavaScript counts them. */
  selectionStart: number;
  selectionEnd: number;
}

interface SolderHttpRequest {
  url: string;
  /** GET when left out. */
  method?: string;
  headers?: Record<string, string>;
  body?: string;
}

interface SolderHttpResponse {
  status: number;
  headers: Record<string, string>;
  body: string;
}

interface Solder {
  /** Runs `handler` for an event the manifest lists under `events`. */
  on<E extends "activate" | "open" | "save" | "change">(
    event: E,
    handler: (event: Extract<SolderEvent, { event: E }>) => void | Promise<void>,
  ): void;
  /** Runs `handler` for a command the manifest lists under `commands`. */
  command(id: string, handler: () => void | Promise<void>): void;
  /** `statusBar`: this plugin's text in the status bar; empty removes it. */
  status(text: string): void;
  /** A line in the plugin's log, shown in the Plugins window. */
  log(...parts: unknown[]): void;
  /** `editor:read`: the file in front. */
  editor(): SolderEditor;
  /**
   * `editor:write`: replaces `start..end` of the file in front, which must
   * still be `path`. The positions are in the text `editor()` last returned,
   * so call it first, and again after an edit.
   */
  edit(path: string, start: number, end: number, text: string): void;
  /** `fs:read`: a file of the project, by its path from the root. */
  readFile(path: string): string;
  /** `http:<host>`: a request to that host. A redirect is not followed. */
  http(request: SolderHttpRequest): SolderHttpResponse;
}

declare const solder: Solder;
declare const console: Record<"log" | "info" | "warn" | "error" | "debug", (...parts: unknown[]) => void>;
