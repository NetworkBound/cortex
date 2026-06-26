import { useEffect, useState } from "react";
import { getApprovals } from "./api";
import { useWs } from "./useWs";

/**
 * Live count of pending approvals, for the Inbox tab badge. Polls every few
 * seconds and bumps on `chat_approval` frames so the badge feels instant.
 */
export function useApprovalCount(): number {
  const [count, setCount] = useState(0);

  useEffect(() => {
    let alive = true;
    const refresh = () =>
      getApprovals()
        .then((a) => alive && setCount(a.length))
        .catch(() => {});
    refresh();
    const t = setInterval(refresh, 5000);
    return () => {
      alive = false;
      clearInterval(t);
    };
  }, []);

  useWs((f) => {
    if (f.type === "chat_approval") setCount((c) => c + 1);
  });

  return count;
}
