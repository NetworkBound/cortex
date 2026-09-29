import type { ImageAttachment } from "@/lib/composer-drop";

/** Vision-attachment chip rack above the textarea, plus skipped-file notes. */
export function ComposerImages({
  images,
  skipped,
  onRemove,
}: {
  images: ImageAttachment[];
  skipped: string[];
  onRemove: (id: string) => void;
}) {
  if (images.length === 0 && skipped.length === 0) return null;
  return (
    <div className="composer-images" role="list" aria-label="Attached images">
      {images.map((img) => (
        <div
          key={img.id}
          className="composer-image-chip"
          role="listitem"
          title={img.name}
        >
          <img className="composer-image-thumb" src={img.dataUrl} alt="" />
          <span className="composer-image-meta">
            <span className="composer-image-name">{img.name}</span>
            <span className="composer-image-size">
              {(img.sizeBytes / 1024).toFixed(0)} KB
            </span>
          </span>
          <button
            type="button"
            className="composer-image-remove"
            onClick={() => onRemove(img.id)}
            title="Remove image"
            aria-label={`Remove ${img.name}`}
          >
            ×
          </button>
        </div>
      ))}
      {skipped.map((s, i) => (
        <span key={`sk-${i}`} className="composer-image-skipped" role="status">
          {s}
        </span>
      ))}
    </div>
  );
}
