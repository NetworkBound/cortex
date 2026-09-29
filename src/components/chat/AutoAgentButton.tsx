import { useState } from "react";
import { Bot, Loader2 } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { humanizeError } from "@/lib/errors";
import { pushToast } from "@/lib/toast";
import { setAgentInstructions } from "@/lib/profiles";
import { useCortexStore } from "@/state/store";

/**
 * "Auto Agent" — generates agent instructions tailored to the active
 * project's stack and saves them as the `agent` profile instructions.
 */
export function AutoAgentButton() {
  const activeProject = useCortexStore((s) => s.activeProject);
  const [generating, setGenerating] = useState(false);

  async function run() {
    if (!activeProject?.root) {
      pushToast({ title: "Open a project first", kind: "warning" });
      return;
    }
    setGenerating(true);
    try {
      const instructions = await invoke<string>("generate_agent_instructions", {
        projectRoot: activeProject.root,
      });
      if (instructions?.trim()) {
        await setAgentInstructions("agent", instructions);
        pushToast({
          title: "Agent instructions generated and saved",
          kind: "success",
        });
      }
    } catch (e) {
      pushToast({ title: humanizeError(e), kind: "error" });
    } finally {
      setGenerating(false);
    }
  }

  return (
    <button
      type="button"
      className={`link-btn auto-agent-btn${generating ? " generating" : ""}`}
      disabled={generating || !activeProject}
      onClick={() => void run()}
      title="Auto-generate agent instructions tailored to the active project's tech stack and structure"
    >
      {generating ? (
        <>
          <Loader2
            size={14}
            strokeWidth={1.75}
            className="spin"
            aria-hidden="true"
          />{" "}
          generating…
        </>
      ) : (
        <>
          <Bot size={14} strokeWidth={1.75} aria-hidden="true" /> Auto Agent
        </>
      )}
    </button>
  );
}
