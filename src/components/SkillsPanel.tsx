/**
 * Skills panel — renders skills loaded from `~/.cortex/skills/<name>/SKILL.md`,
 * lets the user fill in declared inputs and "runs" the skill by expanding the
 * template and appending the rendered prompt into chat as a system message.
 *
 * The "run" verb is a soft one: we don't actually trigger the agent here — we
 * just drop the expanded text into the message stream so the user can review
 * it before sending. That keeps skills as composable prompt scaffolding rather
 * than opaque actions.
 *
 * Input UX:
 *   - Inputs with `options: ['a','b','c']` render as a `<select>`.
 *   - Inputs with no options render as a freeform `<input type="text">`.
 *   - Empty values are *allowed* (passed as empty strings) — only the
 *     template engine complains about missing `{{var}}` references, and any
 *     such error surfaces in the inline status line below the form.
 */

import { useCallback, useEffect, useMemo, useState } from "react";
import { listSkills, expandSkill, type Skill } from "@/lib/skills";
import { PanelLoading } from "./Skeleton";
import { useCortexStore } from "@/state/store";
import { SkillBuilderModal } from "@/components/SkillBuilderModal";

export function SkillsPanel() {
  const [skills, setSkills] = useState<Skill[] | null>(null);
  const [activeName, setActiveName] = useState<string | null>(null);
  const [builderOpen, setBuilderOpen] = useState(false);
  const append = useCortexStore((s) => s.appendMessage);

  const reload = useCallback(async () => {
    const list = await listSkills();
    setSkills(list);
    // Keep the selection if the previously-active skill still exists, else
    // fall back to the first one. The user shouldn't lose their place on
    // every refresh.
    setActiveName((prev) => {
      if (prev && list.some((s) => s.name === prev)) return prev;
      return list[0]?.name ?? null;
    });
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  const active = useMemo(
    () => skills?.find((s) => s.name === activeName) ?? null,
    [skills, activeName],
  );

  function appendSystemMessage(skillName: string, content: string) {
    append({
      id: `skill-${crypto.randomUUID()}`,
      role: "system",
      agent: `skill:${skillName}`,
      content,
      tools: [],
    });
  }

  if (skills === null) {
    return <PanelLoading label="Loading skills" />;
  }
  if (skills.length === 0) {
    return (
      <>
        <div className="muted skills-empty">
          No skills found. Drop a SKILL.md into{" "}
          <code>~/.cortex/skills/&lt;name&gt;/</code> and hit refresh.
          <div className="skills-list-head-actions" style={{ marginTop: "var(--space-3)", justifyContent: "center" }}>
            <button type="button" className="panel-head-action" onClick={() => setBuilderOpen(true)}>
              + New skill
            </button>
            <button type="button" className="panel-head-action ghost" onClick={() => void reload()}>
              Refresh
            </button>
          </div>
        </div>
        {builderOpen && (
          <SkillBuilderModal
            existing={skills}
            onClose={() => setBuilderOpen(false)}
            onSaved={() => void reload()}
          />
        )}
      </>
    );
  }

  return (
    <div className="skills-panel">
      <div className="skills-list">
        <div className="skills-list-head">
          <span className="muted">{skills.length} skill{skills.length === 1 ? "" : "s"}</span>
          <div className="skills-list-head-actions">
            <button type="button" className="panel-head-action" onClick={() => setBuilderOpen(true)}>
              + new
            </button>
            <button type="button" className="panel-head-action ghost" onClick={() => void reload()}>
              Refresh
            </button>
          </div>
        </div>
        {skills.map((s) => (
          <button
            key={s.name}
            type="button"
            className={`skills-row ${activeName === s.name ? "active" : ""}`}
            onClick={() => setActiveName(s.name)}
          >
            <div className="skills-row-name">{s.name}</div>
            {s.description && (
              <div className="skills-row-desc muted">{s.description}</div>
            )}
          </button>
        ))}
      </div>
      <div className="skills-detail">
        {active ? (
          <SkillRunner skill={active} onAppend={appendSystemMessage} />
        ) : (
          <div className="muted skills-empty">Select a skill.</div>
        )}
      </div>
      {builderOpen && (
        <SkillBuilderModal
          existing={skills}
          onClose={() => setBuilderOpen(false)}
          onSaved={() => void reload()}
        />
      )}
    </div>
  );
}

interface SkillRunnerProps {
  skill: Skill;
  onAppend: (skillName: string, content: string) => void;
}

function SkillRunner({ skill, onAppend }: SkillRunnerProps) {
  const [vars, setVars] = useState<Record<string, string>>({});
  const [status, setStatus] = useState<string | null>(null);
  const [running, setRunning] = useState(false);

  // Reset the form whenever the user picks a different skill — otherwise
  // previously-typed values bleed across unrelated templates.
  useEffect(() => {
    const seed: Record<string, string> = {};
    for (const inp of skill.inputs) {
      // Default selects to their first option; leave freeform fields blank.
      seed[inp.name] = inp.options[0] ?? "";
    }
    setVars(seed);
    setStatus(null);
  }, [skill.name, skill.inputs]);

  async function handleRun() {
    setRunning(true);
    setStatus(null);
    try {
      const out = await expandSkill(skill.name, vars);
      if (out === null) {
        setStatus("Failed to expand skill (check console).");
        return;
      }
      onAppend(skill.name, out);
      setStatus("Appended to chat as a system message.");
    } finally {
      setRunning(false);
    }
  }

  return (
    <div className="skills-runner">
      <div className="skills-runner-head">
        <h3 className="skills-runner-title">{skill.name}</h3>
        {skill.description && (
          <p className="skills-runner-desc muted">{skill.description}</p>
        )}
      </div>
      {skill.inputs.length > 0 && (
        <div className="skills-runner-inputs">
          {skill.inputs.map((inp) => (
            <label key={inp.name} className="skills-runner-field">
              <span className="skills-runner-label">{inp.name}</span>
              {inp.options.length > 0 ? (
                <select
                  value={vars[inp.name] ?? ""}
                  onChange={(e) =>
                    setVars((v) => ({ ...v, [inp.name]: e.target.value }))
                  }
                >
                  {inp.options.map((opt) => (
                    <option key={opt} value={opt}>
                      {opt}
                    </option>
                  ))}
                </select>
              ) : (
                <input
                  type="text"
                  value={vars[inp.name] ?? ""}
                  onChange={(e) =>
                    setVars((v) => ({ ...v, [inp.name]: e.target.value }))
                  }
                />
              )}
            </label>
          ))}
        </div>
      )}
      <div className="skills-runner-actions">
        <button
          type="button"
          className="btn-primary"
          disabled={running}
          onClick={() => void handleRun()}
        >
          {running ? "Running…" : "Run skill"}
        </button>
        {status && <span className="skills-runner-status muted">{status}</span>}
      </div>
      <details className="skills-runner-preview">
        <summary>Template preview</summary>
        <pre>{skill.body}</pre>
      </details>
    </div>
  );
}
