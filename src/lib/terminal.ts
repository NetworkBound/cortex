// Thin Tauri-side wrapper for the embedded PTY commands.
//
// Backend handlers live in `src-tauri/src/commands/terminal.rs`. The contract:
//   - `terminal_open(cols, rows)` -> `{ id, child_pid }`
//   - `terminal_write(id, data_b64)` -> ()
//   - `terminal_resize(id, cols, rows)` -> ()
//   - `terminal_close(id)` -> ()
// Bytes flow back to JS as base64 strings on `terminal:output:<id>`.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface PtyHandle {
  id: string;
  child_pid: number;
}

export function openTerminal(cols: number, rows: number): Promise<PtyHandle> {
  return invoke<PtyHandle>("terminal_open", { cols, rows });
}

export function writeTerminal(id: string, data: string): Promise<void> {
  // Encode UTF-8 string -> base64. Used for keystrokes from xterm `onData`.
  const bytes = new TextEncoder().encode(data);
  return invoke("terminal_write", { id, dataB64: bytesToBase64(bytes) });
}

export function resizeTerminal(id: string, cols: number, rows: number): Promise<void> {
  return invoke("terminal_resize", { id, cols, rows });
}

export function closeTerminal(id: string): Promise<void> {
  return invoke("terminal_close", { id });
}

/** Subscribe to raw stdout chunks. Each event carries a base64 string. */
export function onTerminalOutput(id: string, handler: (chunk: Uint8Array) => void): Promise<UnlistenFn> {
  return listen<string>(`terminal:output:${id}`, (evt) => {
    handler(base64ToBytes(evt.payload));
  });
}

/** Subscribe to the "child exited" notification. */
export function onTerminalClosed(id: string, handler: () => void): Promise<UnlistenFn> {
  return listen<void>(`terminal:closed:${id}`, () => handler());
}

function bytesToBase64(bytes: Uint8Array): string {
  // Avoids spreading a potentially huge typed array into apply().
  let binary = "";
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    binary += String.fromCharCode(...bytes.subarray(i, i + chunk));
  }
  return btoa(binary);
}

function base64ToBytes(b64: string): Uint8Array {
  const binary = atob(b64);
  const out = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) out[i] = binary.charCodeAt(i);
  return out;
}
