import { invoke } from "@tauri-apps/api/core";

export interface StoredMessage {
  id: string;
  session_id: string;
  ts: number;
  role: "user" | "assistant" | "system" | "error";
  agent_id: string | null;
  content: string;
  run_id: string | null;
  reasoning: string | null;
  project_root?: string | null;
}

export async function loadSessionMessages(sessionId: string): Promise<StoredMessage[]> {
  return invoke<StoredMessage[]>("load_session_messages", { sessionId });
}

export interface ProjectBootstrap {
  session_id: string;
  messages: StoredMessage[];
  is_resume: boolean;
  context_files_loaded: number;
}

export async function bootstrapProjectSession(projectRoot: string): Promise<ProjectBootstrap> {
  return invoke<ProjectBootstrap>("bootstrap_project_session", { projectRoot });
}

export async function recordMessage(m: {
  id: string;
  sessionId: string;
  role: string;
  agentId?: string | null;
  content: string;
  runId?: string | null;
  reasoning?: string | null;
  projectRoot?: string | null;
}): Promise<void> {
  return invoke("record_message", {
    args: {
      id: m.id,
      session_id: m.sessionId,
      role: m.role,
      agent_id: m.agentId ?? null,
      content: m.content,
      run_id: m.runId ?? null,
      reasoning: m.reasoning ?? null,
      project_root: m.projectRoot ?? null,
    },
  });
}
