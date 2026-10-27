// An off-screen terminal for Ink: a stdout with a settable size that keeps
// every frame (Ink's debug mode writes each frame whole), and a stdin that
// takes writes as keystrokes. The render tests and --smoke use it.

import { EventEmitter } from "node:events";
import type { RenderOptions } from "ink";

export class VirtualStdout extends EventEmitter {
  readonly frames: string[] = [];

  constructor(
    public columns: number,
    public rows: number,
  ) {
    super();
  }

  /** Keeps each frame; calls back as a stream does (Ink's exit waits on an empty write's callback). */
  write = (frame: string, ...rest: unknown[]): boolean => {
    if (frame !== "") this.frames.push(frame);
    const done = rest.find((r) => typeof r === "function") as (() => void) | undefined;
    if (done) setImmediate(done);
    return true;
  };

  lastFrame = (): string => this.frames.at(-1) ?? "";

  /** Changes the size, as a terminal resize does. */
  resize(columns: number, rows: number): void {
    this.columns = columns;
    this.rows = rows;
    this.emit("resize");
  }
}

export class VirtualStdin extends EventEmitter {
  readonly isTTY = true;
  private data: string | null = null;

  /** One chunk, as one read from a terminal delivers it. */
  write(data: string): void {
    this.data = data;
    this.emit("readable");
    this.emit("data", data);
  }

  read = (): string | null => {
    const { data } = this;
    this.data = null;
    return data;
  };

  setEncoding(): void {}
  setRawMode(): void {}
  resume(): void {}
  pause(): void {}
  ref(): void {}
  unref(): void {}
}

export interface VirtualTerminal {
  stdout: VirtualStdout;
  stdin: VirtualStdin;
  /** Ink render options that draw into it. */
  options: RenderOptions;
}

export function virtualTerminal(columns: number, rows: number): VirtualTerminal {
  const stdout = new VirtualStdout(columns, rows);
  const stdin = new VirtualStdin();
  const stderr = new VirtualStdout(columns, rows);
  return {
    stdout,
    stdin,
    options: {
      stdout: stdout as unknown as NodeJS.WriteStream,
      stderr: stderr as unknown as NodeJS.WriteStream,
      stdin: stdin as unknown as NodeJS.ReadStream,
      debug: true,
      exitOnCtrlC: false,
      patchConsole: false,
    },
  };
}
