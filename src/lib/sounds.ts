// Tiny opt-in audio feedback. Synthesised via Web Audio API so we ship no
// audio assets — keeps the bundle small and skips file I/O on startup.
//
// All sounds are gated behind localStorage['cortex.soundsEnabled'] === 'true'
// (default OFF). If the user is muted, has no AudioContext (SSR / locked-down
// webview), or the audio graph throws, we silently no-op — the UI should never
// be impacted by an audio failure.
//
// Design constraints (deliberately conservative):
//   - max gain 0.1 (annoying audio kills the feature)
//   - durations <= 400ms
//   - one shared AudioContext, lazily constructed on first allowed play

export type SoundKind = "done" | "error" | "approve" | "tick";

const STORAGE_KEY = "cortex.soundsEnabled";
const MAX_GAIN = 0.1;

type AudioCtxCtor = typeof AudioContext;
let ctx: AudioContext | null = null;
let unavailable = false;

function getCtx(): AudioContext | null {
  if (unavailable) return null;
  if (ctx) return ctx;
  try {
    const Ctor: AudioCtxCtor | undefined =
      typeof window === "undefined"
        ? undefined
        : (window.AudioContext ??
            (window as unknown as { webkitAudioContext?: AudioCtxCtor }).webkitAudioContext);
    if (!Ctor) {
      unavailable = true;
      return null;
    }
    ctx = new Ctor();
    return ctx;
  } catch {
    unavailable = true;
    return null;
  }
}

function soundsEnabled(): boolean {
  try {
    return localStorage.getItem(STORAGE_KEY) === "true";
  } catch {
    return false;
  }
}

/**
 * Play a short ADSR-shaped oscillator note. Wraps gain in a tiny envelope so
 * notes don't click on start/stop. Always clamped to MAX_GAIN.
 */
function playTone(
  ac: AudioContext,
  freq: number,
  type: OscillatorType,
  startOffset: number,
  durationSec: number,
  peakGain: number,
): void {
  const osc = ac.createOscillator();
  const gain = ac.createGain();
  osc.type = type;
  osc.frequency.value = freq;
  const t0 = ac.currentTime + startOffset;
  const t1 = t0 + durationSec;
  const peak = Math.min(peakGain, MAX_GAIN);
  // Gentle attack/release so we don't get speaker clicks on the edges.
  gain.gain.setValueAtTime(0.0001, t0);
  gain.gain.exponentialRampToValueAtTime(peak, t0 + 0.01);
  gain.gain.exponentialRampToValueAtTime(0.0001, t1);
  osc.connect(gain).connect(ac.destination);
  osc.start(t0);
  osc.stop(t1 + 0.02);
}

function playClick(ac: AudioContext, durationSec: number, peakGain: number): void {
  // 30ms white-noise burst rendered into a one-shot AudioBuffer.
  const sampleRate = ac.sampleRate;
  const frames = Math.max(1, Math.floor(sampleRate * durationSec));
  const buf = ac.createBuffer(1, frames, sampleRate);
  const data = buf.getChannelData(0);
  for (let i = 0; i < frames; i++) data[i] = (Math.random() * 2 - 1) * 0.5;
  const src = ac.createBufferSource();
  const gain = ac.createGain();
  src.buffer = buf;
  const t0 = ac.currentTime;
  const t1 = t0 + durationSec;
  const peak = Math.min(peakGain, MAX_GAIN);
  gain.gain.setValueAtTime(peak, t0);
  gain.gain.exponentialRampToValueAtTime(0.0001, t1);
  src.connect(gain).connect(ac.destination);
  src.start(t0);
  src.stop(t1 + 0.02);
}

export function playSound(kind: SoundKind): void {
  if (!soundsEnabled()) return;
  const ac = getCtx();
  if (!ac) return;
  // Browsers may suspend the context until first user gesture; try to resume
  // but ignore failures — worst case the note is silent.
  if (ac.state === "suspended") {
    void ac.resume().catch(() => {});
  }
  try {
    switch (kind) {
      case "done":
        // C5 → E5, ~280ms total, sine for warmth.
        playTone(ac, 523.25, "sine", 0, 0.13, 0.08);
        playTone(ac, 659.25, "sine", 0.12, 0.16, 0.08);
        break;
      case "error":
        // G3 → E3, square for buzz, ~330ms.
        playTone(ac, 196.0, "square", 0, 0.15, 0.05);
        playTone(ac, 164.81, "square", 0.14, 0.18, 0.05);
        break;
      case "approve":
        // Soft single triangle pluck at A4, ~220ms.
        playTone(ac, 440.0, "triangle", 0, 0.22, 0.08);
        break;
      case "tick":
        // Very short noise click.
        playClick(ac, 0.03, 0.06);
        break;
    }
  } catch {
    // Audio graph is best-effort; never let it surface.
  }
}
