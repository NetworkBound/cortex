import { useEffect, useState } from "react";
import { useCortexStore } from "@/state/store";
import { listModels } from "@/lib/models";

/**
 * Model chip row for inline multi-model compare. Mounted only while the
 * compare toggle is on, so the model universe is fetched lazily on first
 * open (best-effort: failure leaves the list empty). The selection itself
 * lives in the store (`compareModels`, persisted).
 */
export function CompareChips() {
  const compareModels = useCortexStore((s) => s.compareModels);
  const setCompareModels = useCortexStore((s) => s.setCompareModels);
  const [ids, setIds] = useState<string[] | null>(null);

  useEffect(() => {
    let alive = true;
    listModels()
      .then((list) => {
        if (alive) setIds(list.map((m) => m.id));
      })
      .catch(() => {
        if (alive) setIds([]);
      });
    return () => {
      alive = false;
    };
  }, []);

  const atCap = compareModels.length >= 4;
  return (
    <div
      className="compare-chips"
      role="group"
      aria-label="Compare models (pick 2–4)"
    >
      {ids === null ? (
        <span className="compare-hint">loading models…</span>
      ) : ids.length === 0 ? (
        <span className="compare-hint">no models available</span>
      ) : (
        ids.map((id) => {
          const selected = compareModels.includes(id);
          return (
            <button
              key={id}
              type="button"
              className={`compare-chip${selected ? " selected" : ""}`}
              aria-pressed={selected}
              // Cap selection at 4; disable unselected chips once full.
              disabled={!selected && atCap}
              onClick={() =>
                setCompareModels(
                  selected
                    ? compareModels.filter((m) => m !== id)
                    : [...compareModels, id],
                )
              }
            >
              {id}
            </button>
          );
        })
      )}
      {ids !== null && ids.length > 0 && (
        <span className="compare-hint">
          {compareModels.length < 2
            ? "pick ≥2 to compare"
            : `comparing ${compareModels.length} model${compareModels.length === 1 ? "" : "s"}`}
        </span>
      )}
    </div>
  );
}
