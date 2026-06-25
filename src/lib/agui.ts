import { invoke } from "@tauri-apps/api/core";

export async function startAguiServer(bind?: string): Promise<string> {
  return invoke<string>("start_agui_server", { bind: bind ?? null });
}

export async function stopAguiServer(): Promise<boolean> {
  return invoke<boolean>("stop_agui_server");
}
