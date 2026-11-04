// M and :meet (design 2.9): Teams' new-meeting form, prefilled through
// BuildMeetingLink, opened as a link. The prompt is prefilled with the
// subject; enter opens Teams with what it says. Nothing is written, so
// there is nothing to undo.

import { spawn } from "node:child_process";
import { buildMeetingLink, errorReason } from "@todo-tui/client";
import { lineOf } from "../keys/line-editor.js";
import { type Mode, normalMode } from "../state/types.js";
import type { Controller } from "./controller.js";
import { errorText } from "./messages.js";

/** Opens a URL in the browser or app that takes it; rejects when it cannot (tests replace it). */
export type UrlOpener = (url: string) => Promise<void>;

const TEAMS_WEB = "https://teams.microsoft.com/l/";

/**
 * The Teams link as the desktop app takes it: `msteams:/l/…`. A browser
 * given the https link may show web Teams instead of handing it on, which
 * loses the form, so the app's own scheme goes first; `undefined` for any
 * other link.
 */
export function teamsAppLink(url: string): string | undefined {
  return url.startsWith(TEAMS_WEB) ? `msteams:/l/${url.slice(TEAMS_WEB.length)}` : undefined;
}

/** Starts `cmd url` detached; resolves once it runs, so the TUI never waits on a browser. */
function launch(cmd: string, url: string): Promise<void> {
  return new Promise((resolve, reject) => {
    const child = spawn(cmd, [url], { detached: true, stdio: "ignore" });
    child.once("error", (err) => reject(new Error(`${cmd}: ${err.message}`)));
    child.once("spawn", () => {
      child.unref();
      resolve();
    });
  });
}

/** macOS `open url`, waited for: it exits once an app has the link, or non-zero when none takes its scheme. */
function openNow(url: string): Promise<void> {
  return new Promise((resolve, reject) => {
    const child = spawn("open", [url], { stdio: "ignore" });
    const timer = setTimeout(() => {
      child.kill();
      reject(new Error("open: no answer"));
    }, 5000);
    child.once("error", (err) => {
      clearTimeout(timer);
      reject(new Error(`open: ${err.message}`));
    });
    child.once("exit", (code) => {
      clearTimeout(timer);
      if (code === 0) resolve();
      else reject(new Error(`open exited ${code}`));
    });
  });
}

/**
 * `open` on macOS, else `xdg-open`. On macOS a Teams link goes to the
 * desktop app first (`msteams:`), and to the browser when no app takes
 * that scheme.
 */
export const systemOpener: UrlOpener = async (url) => {
  const app = teamsAppLink(url);
  if (process.platform === "darwin") {
    if (app !== undefined) {
      try {
        return await openNow(app);
      } catch {
        // No Teams app: the browser, with the web link.
      }
    }
    return launch("open", url);
  }
  return launch("xdg-open", url);
};

type Prompt = Extract<Mode, { kind: "prompt" }>;

export class Meetings {
  constructor(
    private readonly c: Controller,
    private readonly open: UrlOpener = systemOpener,
  ) {}

  /** M on the selected Item, or the marked ones: the prompt, prefilled with the subject. */
  async start(): Promise<void> {
    const target = this.c.target(true);
    if (!target?.ids.length) return;
    const ids = target.ids;
    let subject: string;
    try {
      subject = (await this.c.pipeline.call(({ timeoutMs }) => buildMeetingLink(this.c.items, ids, timeoutMs !== undefined ? { timeoutMs } : {}))).subject;
    } catch (err) {
      return this.c.failed(errorReason(err) === "MEETINGS_OFF" ? "Meetings are off: [meetings] provider = \"none\"" : `No meeting: ${errorText(err)}`);
    }
    this.c.setMode({ kind: "prompt", prompt: "meeting", target, line: lineOf(subject) });
  }

  /** enter: the link with the subject as edited, opened. */
  async submit(mode: Prompt): Promise<void> {
    const subject = mode.line.text.trim();
    if (!subject) return this.c.setMode({ ...mode, error: "A meeting needs a subject" });
    const ids = mode.target.ids;
    let url: string;
    try {
      url = (await this.c.pipeline.call(({ timeoutMs }) => buildMeetingLink(this.c.items, ids, { subject, ...(timeoutMs !== undefined && { timeoutMs }) }))).url;
    } catch (err) {
      return this.c.failed(`No meeting: ${errorText(err)}`);
    }
    this.c.setMode(normalMode);
    try {
      await this.open(url);
    } catch (err) {
      // The link is still useful: say where it is.
      return this.c.failed(`Could not open Teams (${err instanceof Error ? err.message : String(err)}): ${url}`, { keepEnd: false });
    }
    this.c.toast("info", "Opened the Teams meeting form");
  }
}
