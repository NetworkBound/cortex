/**
 * Thin opener for the memory-entry wizard portal. Lives outside the
 * component file so non-React callers (e.g. slash commands, future
 * menu items) can summon the wizard without pulling in the React
 * render pipeline at import time.
 */

export async function openMemoryWizard(initialTitle?: string): Promise<void> {
  const { openMemoryEntryWizard } = await import("@/components/MemoryEntryWizard");
  openMemoryEntryWizard(initialTitle);
}
