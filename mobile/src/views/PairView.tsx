// First launch: "Connect to your Cortex". Paste URL + 6-digit code, scan the
// desktop's QR (native bridge, or BarcodeDetector where the browser has it),
// or try the built-in demo.

import { useEffect, useRef, useState } from "react";
import Icon from "../components/Icon";
import { Banner, Field, Sheet, Spinner } from "../components/ui";
import * as api from "../lib/api";
import { errorMessage } from "../lib/http";
import { deviceName, haptic, isNativeShell, native } from "../lib/native";
import { useRoute } from "../lib/nav";
import { normaliseUrl, parsePairLink, session } from "../lib/session";
import { useStore } from "../lib/store";

type Detector = {
  detect: (src: ImageBitmapSource) => Promise<{ rawValue: string }[]>;
};
type DetectorCtor = new (opts: { formats: string[] }) => Detector;

function barcodeDetector(): DetectorCtor | null {
  const w = window as Window & { BarcodeDetector?: DetectorCtor };
  return w.BarcodeDetector ?? null;
}

export default function PairView() {
  const { boot, bootError, completePairing, reprobe, startDemo, toast } =
    useStore();
  const route = useRoute();
  const sameOrigin = !isNativeShell();
  const [url, setUrl] = useState(
    () => session.server().url || (sameOrigin ? location.origin : ""),
  );
  const [code, setCode] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [scan, setScan] = useState(false);
  const canScan = !!native()?.scanQr || !!barcodeDetector();

  // Deep link `#/pair?url=&code=` (or cortex://pair) prefills + auto-connects.
  const auto = useRef(false);
  useEffect(() => {
    if (route.tab !== "chats" && route.path !== "/pair") return;
    const u = route.params.get("url");
    const c = route.params.get("code");
    if (u) setUrl(normaliseUrl(u));
    if (c) setCode(c);
    if (c && !auto.current) {
      auto.current = true;
      setTimeout(() => connect(u ? normaliseUrl(u) : undefined, c), 0);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [route.path, route.params]);

  const connect = async (u = url, c = code) => {
    const base = sameOrigin && !u ? "" : normaliseUrl(u);
    const digits = c.replace(/\D/g, "");
    if (!sameOrigin && !base) {
      setError("Enter your Cortex address (shown in Settings → Mobile).");
      return;
    }
    if (digits.length !== 6) {
      setError("The pairing code is 6 digits.");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      // Point the client at the server first so the pair call goes there.
      await session.setServer(
        { url: sameOrigin && base === location.origin ? "" : base },
        null,
      );
      const r = await api.pair(digits, await deviceName());
      haptic("success");
      await completePairing(
        {
          url: sameOrigin && base === location.origin ? "" : base,
          device_id: r.device_id,
          server_name: r.server_name,
          server_version: r.server_version,
        },
        r.token,
      );
      try {
        sessionStorage.setItem("cortex.justPaired", "1");
      } catch {
        /* ignore */
      }
    } catch (e) {
      haptic("error");
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  const onScanned = (raw: string) => {
    const p = parsePairLink(raw);
    if (!p) {
      toast("That QR code isn't a Cortex pairing code.", "error");
      return;
    }
    if (p.url) setUrl(p.url);
    if (p.code) setCode(p.code);
    setScan(false);
    if (p.code) connect(p.url ?? url, p.code);
  };

  const scanNative = async () => {
    const n = native();
    if (n?.scanQr) {
      try {
        const r = await n.scanQr();
        if (r) onScanned(r);
      } catch (e) {
        toast(errorMessage(e), "error");
      }
      return;
    }
    setScan(true);
  };

  const onPasteCode = (v: string) => {
    // Pasting the whole cortex://pair?… link into the code box also works.
    const p = parsePairLink(v);
    if (p && (p.url || p.code)) {
      if (p.url) setUrl(p.url);
      setCode(p.code ?? "");
      return;
    }
    setCode(v.replace(/\D/g, "").slice(0, 6));
  };

  return (
    <div className="pair">
      <div className="pair-hero">
        <span className="brand-mark">C</span>
        <h1>Connect to your Cortex</h1>
        <p className="muted">
          On the desktop open <b>Settings → Mobile</b>, then scan the QR code or
          type the 6-digit code it shows.
        </p>
      </div>

      {boot === "unreachable" && (
        <Banner
          kind="error"
          action={{ label: "Retry", onClick: () => reprobe() }}
        >
          Can't reach {session.server().url || "the server"}.{" "}
          {bootError ? `(${bootError})` : ""}
        </Banner>
      )}
      {error && <Banner kind="error">{error}</Banner>}

      <div className="pair-form">
        {!sameOrigin && (
          <Field
            label="Cortex address"
            hint="Tailscale name or LAN IP, e.g. desktop.tail1234.ts.net:8788"
          >
            <input
              type="url"
              inputMode="url"
              autoCapitalize="off"
              autoCorrect="off"
              placeholder="http://desktop.tail1234.ts.net:8788"
              value={url}
              onChange={(e) => setUrl(e.target.value)}
            />
          </Field>
        )}
        <Field label="Pairing code">
          <input
            className="code-input"
            inputMode="numeric"
            autoComplete="one-time-code"
            pattern="[0-9]*"
            maxLength={40}
            placeholder="123456"
            value={code}
            onChange={(e) => onPasteCode(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && connect()}
          />
        </Field>
        <button
          className="btn primary block"
          disabled={busy}
          onClick={() => connect()}
        >
          {busy ? <Spinner small /> : "Connect"}
        </button>
        {canScan && (
          <button className="btn block" onClick={scanNative} disabled={busy}>
            <Icon name="qr" size={18} /> Scan QR code
          </button>
        )}
        <button className="linkbtn block" onClick={() => startDemo()}>
          Try the demo without a server
        </button>
      </div>

      <Sheet
        open={scan}
        onClose={() => setScan(false)}
        title="Scan QR code"
        tall
      >
        {scan && <CameraScanner onResult={onScanned} />}
      </Sheet>
    </div>
  );
}

/** In-browser scanner using BarcodeDetector (Chrome/Android). Safari has no
 *  detector, so there the native bridge or manual entry is the path. */
function CameraScanner({ onResult }: { onResult: (raw: string) => void }) {
  const video = useRef<HTMLVideoElement | null>(null);
  const [err, setErr] = useState<string | null>(null);
  useEffect(() => {
    const Ctor = barcodeDetector();
    if (!Ctor) {
      setErr("This browser can't scan QR codes. Type the code instead.");
      return;
    }
    let stream: MediaStream | null = null;
    let timer: ReturnType<typeof setInterval> | null = null;
    let done = false;
    (async () => {
      try {
        stream = await navigator.mediaDevices.getUserMedia({
          video: { facingMode: "environment" },
          audio: false,
        });
        const el = video.current;
        if (!el) return;
        el.srcObject = stream;
        await el.play();
        const det = new Ctor({ formats: ["qr_code"] });
        timer = setInterval(async () => {
          if (done || !video.current || video.current.readyState < 2) return;
          try {
            const codes = await det.detect(video.current);
            const hit = codes.find((c) => c.rawValue);
            if (hit) {
              done = true;
              haptic("success");
              onResult(hit.rawValue);
            }
          } catch {
            /* keep scanning */
          }
        }, 300);
      } catch (e) {
        setErr(`Camera unavailable: ${errorMessage(e)}`);
      }
    })();
    return () => {
      done = true;
      if (timer) clearInterval(timer);
      stream?.getTracks().forEach((t) => t.stop());
    };
  }, [onResult]);
  return (
    <div className="scanner">
      {err ? (
        <Banner kind="error">{err}</Banner>
      ) : (
        <video ref={video} playsInline muted />
      )}
      <p className="muted small">
        Point the camera at the QR in Settings → Mobile.
      </p>
    </div>
  );
}
