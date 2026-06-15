// Composer drag-drop helpers.
//
// Three buckets:
//   image/*               → base64 data-URL, embedded as ![name](data:...)
//   text-like ≤ 200 KB    → fenced ```ext block prepended to input
//   everything else       → @filename placeholder reference
//
// We cap text at 200 KB to keep model context lean; binaries never get
// base64-embedded (too noisy for chat). FileReader is built-in — no deps.
//
// Vision support (Terax #15): separate `extractImageAttachments` returns
// structured `ImageAttachment` records that the composer renders as chip
// thumbnails above the textarea and forwards as a `images: string[]`
// argument to `chat_send`, rather than inlining base64 into the message
// text. Caps: 5 MB each, max 3 per send, anthropic-supported MIME types only.

import { humanizeError } from "@/lib/errors";

export const TEXT_CAP = 200 * 1024;
export const IMAGE_BYTES_CAP = 5 * 1024 * 1024;
export const IMAGE_COUNT_CAP = 3;

/** MIME types the Anthropic vision API accepts. */
export const IMAGE_MIME_ALLOWLIST = new Set([
  "image/png",
  "image/jpeg",
  "image/webp",
  "image/gif",
]);

/** Extensions we accept when `File.type` is empty (drag from filesystem on linux). */
export const IMAGE_EXT_TO_MIME: Record<string, string> = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  webp: "image/webp",
  gif: "image/gif",
};

export interface ImageAttachment {
  /** Stable id for keying chip elements in React. */
  id: string;
  /** Original filename (best-effort — drop sources don't always preserve it). */
  name: string;
  /** Canonicalized MIME type (one of `IMAGE_MIME_ALLOWLIST`). */
  mediaType: string;
  /** Full base64 data URI — `data:image/png;base64,…`. Used both as thumbnail src and wire payload. */
  dataUrl: string;
  /** Byte length, post-decode. Used for the chip's size label. */
  sizeBytes: number;
}

export interface ImageExtractionResult {
  /** Accepted attachments, capped to `IMAGE_COUNT_CAP`. */
  attachments: ImageAttachment[];
  /** Human-readable reasons for any dropped files (wrong type, too large, over count cap). */
  skipped: string[];
}

// Map of recognized extensions → markdown fence language tag. Edit this to
// add new file types; falls back to the raw ext if missing.
export const TEXT_EXTS: Record<string, string> = {
  ts: "ts", tsx: "tsx", js: "js", jsx: "jsx", py: "py", rs: "rs",
  go: "go", md: "md", json: "json", yaml: "yaml", yml: "yaml",
  toml: "toml", html: "html", css: "css", sh: "sh", sql: "sql",
};

export function extOf(name: string): string {
  const i = name.lastIndexOf(".");
  return i >= 0 ? name.slice(i + 1).toLowerCase() : "";
}

function readAsText(file: File): Promise<string> {
  return new Promise((res, rej) => {
    const r = new FileReader();
    r.onload = () => res(String(r.result ?? ""));
    r.onerror = () => rej(r.error);
    r.readAsText(file);
  });
}

function readAsDataURL(file: File): Promise<string> {
  return new Promise((res, rej) => {
    const r = new FileReader();
    r.onload = () => res(String(r.result ?? ""));
    r.onerror = () => rej(r.error);
    r.readAsDataURL(file);
  });
}

// Convert one dropped File into a chunk of composer text. Errors degrade to a
// safe `@filename` reference rather than throwing — a single bad drop should
// never lose the rest of the batch.
export async function fileToComposerChunk(file: File): Promise<string> {
  const ext = extOf(file.name);
  const isImage = file.type.startsWith("image/");
  const isTextish =
    file.size <= TEXT_CAP &&
    (file.type.startsWith("text/") || ext in TEXT_EXTS);
  try {
    if (isImage) {
      const url = await readAsDataURL(file);
      return `![${file.name}](${url})`;
    }
    if (isTextish) {
      const body = await readAsText(file);
      const lang = TEXT_EXTS[ext] ?? ext ?? "";
      return "```" + lang + "\n" + body + "\n```";
    }
    return `@${file.name}`;
  } catch {
    return `@${file.name}`;
  }
}

// Batch-convert a FileList into a single insertion string. Returns "" when
// the list is empty — callers can early-return without an extra length check.
export async function filesToComposerText(files: FileList): Promise<string> {
  const arr = Array.from(files);
  if (arr.length === 0) return "";
  const pieces = await Promise.all(arr.map(fileToComposerChunk));
  return pieces.join("\n\n");
}

/** Resolve a `File`'s MIME type, falling back to its extension for OS-level drops. */
function resolveImageMime(file: File): string | null {
  const t = file.type.toLowerCase();
  if (IMAGE_MIME_ALLOWLIST.has(t)) return t;
  if (t === "") {
    const ext = extOf(file.name);
    const guessed = IMAGE_EXT_TO_MIME[ext];
    if (guessed) return guessed;
  }
  return null;
}

/**
 * Pluck image files (png/jpg/jpeg/webp/gif) out of a drop / paste and read
 * each as a base64 data URI. Non-image files are ignored — callers should
 * also run `filesToComposerText` for the rest. Already-attached images
 * (passed as `existing`) count toward the per-message cap.
 */
export async function extractImageAttachments(
  files: FileList | File[],
  existing: ImageAttachment[] = [],
): Promise<ImageExtractionResult> {
  const arr = Array.from(files);
  const skipped: string[] = [];
  const accepted: ImageAttachment[] = [];
  let budget = Math.max(0, IMAGE_COUNT_CAP - existing.length);

  for (const file of arr) {
    const mime = resolveImageMime(file);
    if (!mime) continue; // not an image — caller handles via text/file path
    if (budget <= 0) {
      skipped.push(`${file.name}: max ${IMAGE_COUNT_CAP} images per message`);
      continue;
    }
    if (file.size > IMAGE_BYTES_CAP) {
      const mb = (file.size / (1024 * 1024)).toFixed(1);
      skipped.push(`${file.name}: ${mb} MB exceeds 5 MB limit`);
      continue;
    }
    try {
      const dataUrl = await readAsDataURL(file);
      accepted.push({
        id: `img-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
        name: file.name || "pasted-image",
        mediaType: mime,
        dataUrl,
        sizeBytes: file.size,
      });
      budget -= 1;
    } catch (err) {
      skipped.push(`${file.name}: read failed (${humanizeError(err)})`);
    }
  }

  return { attachments: accepted, skipped };
}

/** Strip the `data:<mime>;base64,` prefix; returns `null` if shape is unexpected. */
export function dataUrlToBase64(dataUrl: string): { mediaType: string; base64: string } | null {
  const m = dataUrl.match(/^data:([^;,]+);base64,(.+)$/);
  if (!m) return null;
  return { mediaType: m[1], base64: m[2] };
}
