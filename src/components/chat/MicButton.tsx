import { useEffect, useRef, useState } from "react";
import { Loader2, Mic, Square } from "lucide-react";
import { humanizeError } from "@/lib/errors";
import { pushToast } from "@/lib/toast";
import {
  recordAndTranscribe,
  type RecordAndTranscribeHandle,
} from "@/lib/voice-fallback";

/**
 * Voice-to-text composer button: browser SpeechRecognition first, whisper-cli
 * fallback (`recordAndTranscribe`). Inserts the transcript at the caret.
 */
export function MicButton({
  onTranscript,
}: {
  onTranscript: (text: string) => void;
}) {
  type MicState = "idle" | "recording" | "busy";
  const [state, setState] = useState<MicState>("idle");
  const handleRef = useRef<RecordAndTranscribeHandle | null>(null);
  const recRef = useRef<{ stop(): void } | null>(null);

  useEffect(() => {
    return () => {
      handleRef.current?.stop();
      handleRef.current = null;
      recRef.current?.stop();
      recRef.current = null;
    };
  }, []);

  const startBrowser = () => {
    const w = window as unknown as {
      SpeechRecognition?: new () => {
        lang: string;
        interimResults: boolean;
        onresult:
          | ((ev: {
              results: ArrayLike<ArrayLike<{ transcript: string }>>;
            }) => void)
          | null;
        onerror: ((ev: { error?: string }) => void) | null;
        onend: (() => void) | null;
        start(): void;
        stop(): void;
      };
      webkitSpeechRecognition?: typeof w.SpeechRecognition;
    };
    const Ctor = w.SpeechRecognition ?? w.webkitSpeechRecognition;
    if (!Ctor) return false;
    try {
      const rec = new Ctor();
      rec.lang = navigator.language || "en-US";
      rec.interimResults = false;
      rec.onresult = (ev) => {
        const text = (ev.results[0]?.[0]?.transcript ?? "").trim();
        recRef.current = null;
        setState("idle");
        if (text) onTranscript(text);
        else
          pushToast({
            title: "Voice",
            body: "No speech captured.",
            kind: "info",
          });
      };
      rec.onerror = (ev) => {
        recRef.current = null;
        setState("idle");
        pushToast({
          title: "Voice error",
          body: ev.error ?? "unknown",
          kind: "warning",
        });
      };
      rec.onend = () => {
        recRef.current = null;
        setState("idle");
      };
      rec.start();
      recRef.current = rec;
      setState("recording");
      return true;
    } catch {
      return false;
    }
  };

  const startWhisper = () => {
    let handle: RecordAndTranscribeHandle;
    try {
      handle = recordAndTranscribe();
    } catch (e) {
      pushToast({
        title: "Voice unavailable",
        body: humanizeError(e),
        kind: "warning",
      });
      return;
    }
    handleRef.current = handle;
    setState("recording");
    handle.promise.then(
      (transcript) => {
        handleRef.current = null;
        setState("idle");
        const text = transcript.trim();
        if (!text) {
          pushToast({
            title: "Voice",
            body: "No speech captured.",
            kind: "info",
          });
          return;
        }
        onTranscript(text);
      },
      (err) => {
        handleRef.current = null;
        setState("idle");
        pushToast({
          title: "Voice failed",
          body: humanizeError(err),
          kind: "warning",
        });
      },
    );
  };

  const start = () => {
    if (!startBrowser()) startWhisper();
  };

  const stop = () => {
    if (recRef.current) {
      recRef.current.stop();
      recRef.current = null;
      setState("idle");
    } else if (handleRef.current) {
      handleRef.current.stop();
      setState("busy");
    }
  };

  const onClick = () => {
    if (state === "idle") start();
    else if (state === "recording") stop();
  };

  const Icon =
    state === "recording" ? Square : state === "busy" ? Loader2 : Mic;

  return (
    <button
      type="button"
      className={`quick-attach-btn composer-mic${state === "recording" ? " recording" : ""}`}
      onClick={onClick}
      disabled={state === "busy"}
      aria-pressed={state === "recording"}
      title={
        state === "recording"
          ? "Recording — click to stop"
          : state === "busy"
            ? "Transcribing…"
            : "Record voice and insert transcript"
      }
    >
      <Icon
        size={14}
        strokeWidth={1.75}
        aria-hidden="true"
        className={state === "busy" ? "spin" : undefined}
      />{" "}
      {state === "recording"
        ? "stop"
        : state === "busy"
          ? "transcribing"
          : "mic"}
    </button>
  );
}
