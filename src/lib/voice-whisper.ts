/**
 * Whisper.cpp transcription fallback for the `/voice` slash command.
 *
 * The primary path (browser `SpeechRecognition`) handles 99% of cases on
 * Chrome/Edge. When it's missing or fails, callers can fall back through
 * `transcribeAudio(blob)` — we base64 the wav and ship it to the Rust
 * `voice_transcribe` command, which shells out to a locally-installed
 * `whisper-cli` binary.
 *
 * NOTE: This module is intentionally *not* wired into the slash command
 * yet — it's the shipped backend so a future opt-in can swap it in
 * without touching this file.
 */
import { invoke } from "@tauri-apps/api/core";

/** Base64-encode a Blob, stripping the `data:...;base64,` prefix. */
async function blobToBase64(blob: Blob): Promise<string> {
  const buf = await blob.arrayBuffer();
  const bytes = new Uint8Array(buf);
  // Chunked conversion to avoid the call-stack limit on `String.fromCharCode`
  // when the recording is more than ~100 KB.
  let binary = "";
  const CHUNK = 0x8000;
  for (let i = 0; i < bytes.length; i += CHUNK) {
    const slice = bytes.subarray(i, i + CHUNK);
    for (let j = 0; j < slice.length; j++) {
      binary += String.fromCharCode(slice[j]);
    }
  }
  return btoa(binary);
}

/**
 * Transcribe a recorded audio blob via whisper.cpp.
 *
 * Resolves with the trimmed transcript on success. Rejects with the
 * backend's error message when whisper-cli isn't installed, the model
 * file is missing, or the CLI itself fails.
 */
export async function transcribeAudio(blob: Blob): Promise<string> {
  const audio_b64 = await blobToBase64(blob);
  return await invoke<string>("voice_transcribe", { audioB64: audio_b64 });
}
