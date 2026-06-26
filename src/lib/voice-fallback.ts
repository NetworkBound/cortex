/**
 * Voice capture + transcribe fallback helper.
 *
 * Wraps `transcribeAudio` from `voice-whisper.ts` with a `MediaRecorder`
 * front-end so the `/voice` slash command (or any consumer) can capture
 * a short clip from the user's microphone and ship it to the locally
 * installed whisper.cpp pipeline without needing to know anything about
 * the audio plumbing.
 *
 * Usage:
 *
 *   const { promise, stop } = recordAndTranscribe();
 *   button.onclick = () => stop();         // user-driven stop
 *   const transcript = await promise;      // resolves once whisper returns
 *
 * The promise also auto-resolves on a 4s safety timeout so a forgotten
 * recording can't pin the mic open forever. If MediaRecorder isn't
 * available (no getUserMedia, no MediaRecorder, denied permission), the
 * promise rejects with an explanatory error so callers can surface a
 * useful message rather than a stack trace.
 *
 * NOTE: This file is intentionally NOT wired into the /voice slash
 * command in this commit — see the follow-up that swaps the
 * SpeechRecognition path. Shipping the helper standalone keeps this
 * change reviewable.
 */
import { transcribeAudio } from "./voice-whisper";

/** Result of `recordAndTranscribe` — the caller awaits `promise` for the
 *  final transcript and can call `stop()` to end recording early. */
export interface RecordAndTranscribeHandle {
  /** Resolves with the trimmed transcript once whisper returns. */
  promise: Promise<string>;
  /** End the recording now — the promise resolves shortly after. */
  stop: () => void;
}

/** Max length of a single recording, in milliseconds. The /voice command
 *  is meant for short utterances; this is a safety cap so a stuck recorder
 *  doesn't hand whisper an unbounded blob and block the UI forever. It must
 *  be long enough to never truncate a normal spoken utterance, so it is set
 *  to 30s rather than a few seconds. */
const RECORD_TIMEOUT_MS = 30000;

/**
 * Capture a short audio clip from the default input device and transcribe
 * it via whisper.cpp.
 *
 * Returns an object exposing the in-flight promise plus an early-stop
 * function. The promise:
 *   - resolves with the transcript on success.
 *   - rejects with a descriptive Error when MediaRecorder is unavailable
 *     or whisper itself errors out.
 */
export function recordAndTranscribe(): RecordAndTranscribeHandle {
  // Probe both halves of the API up front. Some browsers (and Tauri's
  // older webview on Linux) expose `getUserMedia` without MediaRecorder,
  // which would otherwise blow up halfway through the flow.
  if (
    typeof navigator === "undefined" ||
    !navigator.mediaDevices ||
    typeof navigator.mediaDevices.getUserMedia !== "function" ||
    typeof MediaRecorder === "undefined"
  ) {
    const err = new Error(
      "Voice capture isn't available in this environment — MediaRecorder/getUserMedia missing.",
    );
    return {
      promise: Promise.reject(err),
      stop: () => {
        /* no-op */
      },
    };
  }

  let recorder: MediaRecorder | null = null;
  let stream: MediaStream | null = null;
  let stopped = false;
  let timeoutId: ReturnType<typeof setTimeout> | null = null;

  const stop = () => {
    if (stopped) return;
    stopped = true;
    if (timeoutId !== null) {
      clearTimeout(timeoutId);
      timeoutId = null;
    }
    if (recorder && recorder.state !== "inactive") {
      try {
        recorder.stop();
      } catch {
        /* recorder may already be stopping */
      }
    }
  };

  const cleanupStream = () => {
    if (stream) {
      for (const track of stream.getTracks()) {
        try {
          track.stop();
        } catch {
          /* ignore — track may already be ended */
        }
      }
      stream = null;
    }
  };

  const promise = new Promise<string>((resolve, reject) => {
    navigator.mediaDevices
      .getUserMedia({ audio: true })
      .then((s) => {
        stream = s;
        // If the caller called stop() before the mic was granted, bail
        // immediately rather than starting a recorder we'll never use.
        if (stopped) {
          cleanupStream();
          reject(new Error("Recording cancelled before mic was granted."));
          return;
        }

        const chunks: Blob[] = [];
        try {
          recorder = new MediaRecorder(s);
        } catch (e) {
          cleanupStream();
          reject(
            new Error(
              `Failed to construct MediaRecorder: ${e instanceof Error ? e.message : String(e)}`,
            ),
          );
          return;
        }

        recorder.ondataavailable = (ev: BlobEvent) => {
          if (ev.data && ev.data.size > 0) chunks.push(ev.data);
        };
        recorder.onerror = (ev: Event) => {
          cleanupStream();
          const detail =
            ev instanceof ErrorEvent && ev.error
              ? String(ev.error)
              : "unknown MediaRecorder error";
          reject(new Error(`MediaRecorder failed: ${detail}`));
        };
        recorder.onstop = () => {
          cleanupStream();
          // Pull the recorder's mime type when present so the resulting
          // Blob is tagged correctly (whisper-cli handles webm/ogg/wav).
          const type = recorder?.mimeType || "audio/webm";
          const blob = new Blob(chunks, { type });
          if (blob.size === 0) {
            reject(new Error("Empty recording — nothing captured."));
            return;
          }
          transcribeAudio(blob).then(resolve, (err) => {
            reject(
              err instanceof Error
                ? err
                : new Error(typeof err === "string" ? err : String(err)),
            );
          });
        };

        recorder.start();
        timeoutId = setTimeout(stop, RECORD_TIMEOUT_MS);
      })
      .catch((err) => {
        cleanupStream();
        const msg = err instanceof Error ? err.message : String(err);
        reject(new Error(`Microphone access denied or unavailable: ${msg}`));
      });
  });

  return { promise, stop };
}
