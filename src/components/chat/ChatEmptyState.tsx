import { FileDiff, Map as MapIcon, TestTube2, Sparkles } from "lucide-react";

interface ExamplePrompt {
  title: string;
  prompt: string;
  icon: typeof FileDiff;
  /** Needs an open project (uses @diff / @repomap / the test runner). */
  needsProject?: boolean;
}

const PROJECT_PROMPTS: ExamplePrompt[] = [
  {
    title: "Tour this codebase",
    prompt:
      "@repomap Give me a tour of this codebase: the key modules, entry points, and how they fit together.",
    icon: MapIcon,
    needsProject: true,
  },
  {
    title: "Review my changes",
    prompt:
      "@diff Review these changes for bugs, edge cases and anything I should test before committing.",
    icon: FileDiff,
    needsProject: true,
  },
  {
    title: "Fix the failing tests",
    prompt:
      "Run the test suite, find what is failing, and propose a fix. Show me the diff before applying it.",
    icon: TestTube2,
    needsProject: true,
  },
];

const GENERAL_PROMPTS: ExamplePrompt[] = [
  {
    title: "Plan a feature",
    prompt:
      "Help me plan a new feature: ask me clarifying questions, then write a step-by-step implementation plan.",
    icon: MapIcon,
  },
  {
    title: "Explain a concept",
    prompt:
      "Explain how OAuth 2.0 PKCE works, with a short sequence diagram and the common pitfalls.",
    icon: Sparkles,
  },
  {
    title: "Draft a script",
    prompt:
      "Write a small script that renames files in a folder to kebab-case, with a dry-run flag. Ask which language first.",
    icon: TestTube2,
  },
];

const TOKEN_HINTS = ["@brain", "@diff", "@status", "@recent"];
const COMMAND_HINTS = ["/summarize", "/test", "/help"];

export interface ChatEmptyStateProps {
  hasProject: boolean;
  /** Put a full prompt in the composer (and focus it). */
  onUsePrompt: (text: string) => void;
  /** Splice an @-token at the composer caret. */
  onInsertToken: (token: string) => void;
  onBrowse: () => void;
}

/**
 * New-chat landing: brand mark, one line of what Cortex is, three example
 * prompts (project-aware), the @-token / slash-command hint chips and the two
 * shortcuts worth knowing. Everything clickable is a real button.
 */
export function ChatEmptyState({
  hasProject,
  onUsePrompt,
  onInsertToken,
  onBrowse,
}: ChatEmptyStateProps) {
  const prompts = hasProject ? PROJECT_PROMPTS : GENERAL_PROMPTS;
  return (
    <div className="chat-empty">
      <div className="chat-empty-logo" aria-hidden="true">
        C
      </div>
      <h2>Cortex</h2>
      <p className="chat-empty-sub">
        One chat, every model. Switch between Claude, Codex, Gemini and more
        without leaving the conversation.
      </p>

      <div className="chat-empty-prompts" role="list" aria-label="Try asking">
        {prompts.map((p) => {
          const Icon = p.icon;
          return (
            <button
              key={p.title}
              type="button"
              role="listitem"
              className="chat-empty-prompt"
              onClick={() => onUsePrompt(p.prompt)}
              title={p.prompt}
            >
              <Icon size={14} strokeWidth={1.75} aria-hidden="true" />
              <span className="chat-empty-prompt-title">{p.title}</span>
              <span className="chat-empty-prompt-text">{p.prompt}</span>
            </button>
          );
        })}
      </div>
      {!hasProject && (
        <p className="chat-empty-hint">
          Open a project from the sidebar to unlock <code>@diff</code>,{" "}
          <code>@repomap</code> and file context.
        </p>
      )}

      <div className="chat-empty-discover">
        <div className="chat-empty-hint-row">
          <span className="chat-empty-hint">Context:</span>
          {TOKEN_HINTS.map((t) => (
            <button
              key={t}
              type="button"
              className="chat-empty-token"
              onClick={() => onInsertToken(t)}
              title={`Insert ${t} into the composer`}
            >
              {t}
            </button>
          ))}
          <span className="chat-empty-hint">
            — type <code>@</code> for the full picker
          </span>
        </div>
        <div className="chat-empty-hint-row">
          <span className="chat-empty-hint">Commands:</span>
          {COMMAND_HINTS.map((c) => (
            <button
              key={c}
              type="button"
              className="chat-empty-token"
              onClick={() => onUsePrompt(c)}
              title={`Put ${c} in the composer`}
            >
              {c}
            </button>
          ))}
          <span className="chat-empty-hint">
            — type <code>/</code> to see them all
          </span>
        </div>
        <ul className="chat-empty-tips">
          <li>
            <span className="chat-empty-tip-label">Send message</span>
            <span className="chat-empty-keys">
              <kbd>Ctrl</kbd>+<kbd>Enter</kbd>
            </span>
          </li>
          <li>
            <span className="chat-empty-tip-label">Command palette</span>
            <span className="chat-empty-keys">
              <kbd>Ctrl</kbd>+<kbd>K</kbd>
            </span>
          </li>
        </ul>
        <button
          type="button"
          className="chat-empty-browse"
          onClick={onBrowse}
          title="Browse all features (Ctrl+K)"
        >
          Browse all features
          <span className="chat-empty-browse-hint">Ctrl+K</span>
        </button>
      </div>
    </div>
  );
}
