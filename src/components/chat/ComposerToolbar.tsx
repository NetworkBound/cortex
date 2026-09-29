import { useRef } from "react";
import {
  Brain,
  Columns2,
  FileDiff,
  FileText,
  History,
  Loader2,
  Paperclip,
  Sparkles,
  Wand2,
  Zap,
} from "lucide-react";
import { ModelPicker } from "../ModelPicker";
import { ReasoningPicker } from "../ReasoningPicker";
import { MicButton } from "./MicButton";
import { AutoAgentButton } from "./AutoAgentButton";
import { previewAttachments } from "./attachment-tokens";

const FILE_ACCEPT =
  "image/png,image/jpeg,image/webp,image/gif,.ts,.tsx,.js,.jsx,.py,.rs,.go,.md,.json,.yaml,.yml,.toml,.html,.css,.sh,.sql,.txt,.csv,.xml,.log";

export interface ComposerToolbarProps {
  input: string;
  sending: boolean;
  brainThinking: boolean;
  suggestingContext: boolean;
  enhancing: boolean;
  enhancePrompt: boolean;
  compareOn: boolean;
  /** Splice an @-token (or transcript) at the caret. */
  onInsert: (token: string) => void;
  /** Replace the whole draft (slash-command shortcuts). */
  onSetInput: (value: string) => void;
  onPickFiles: (files: FileList) => void;
  onSuggestContext: () => void;
  onToggleCompare: () => void;
  onToggleEnhance: () => void;
  onSend: () => void;
}

/**
 * The row under the textarea: quick-attach chips, the attachment preview,
 * model/reasoning pickers, compare / enhance / Auto Agent toggles and the
 * Send (or Queue) button pinned bottom-right.
 */
export function ComposerToolbar({
  input,
  sending,
  brainThinking,
  suggestingContext,
  enhancing,
  enhancePrompt,
  compareOn,
  onInsert,
  onSetInput,
  onPickFiles,
  onSuggestContext,
  onToggleCompare,
  onToggleEnhance,
  onSend,
}: ComposerToolbarProps) {
  const fileInputRef = useRef<HTMLInputElement>(null);
  // Pre-send attachment preview: the @-tokens and implicit path mentions the
  // backend `expand_at_tokens` will resolve, so the user sees "N attachments
  // queued" before hitting send.
  const attachments = previewAttachments(input);

  return (
    <div className="chat-input-actions">
      <div className="chat-input-tools">
        <div
          className="quick-attach"
          role="toolbar"
          aria-label="Quick-attach context"
        >
          <button
            type="button"
            className="quick-attach-btn"
            onClick={() => onInsert("@brain")}
            title="Auto-attach top 3 brain hits for this message"
            aria-label="Attach top brain hits (@brain)"
          >
            <Brain size={14} strokeWidth={1.75} aria-hidden="true" /> brain
          </button>
          <button
            type="button"
            className="quick-attach-btn"
            onClick={() => onInsert("@diff")}
            title="Attach git diff vs HEAD of active project"
            aria-label="Attach git diff (@diff)"
          >
            <FileDiff size={14} strokeWidth={1.75} aria-hidden="true" /> diff
          </button>
          <button
            type="button"
            className="quick-attach-btn"
            onClick={() => onInsert("@recent")}
            title="Attach last 8 modified files in active project"
            aria-label="Attach recent files (@recent)"
          >
            <History size={14} strokeWidth={1.75} aria-hidden="true" /> recent
          </button>
          <button
            type="button"
            className="quick-attach-btn"
            onClick={() => onSetInput("/summarize")}
            aria-label="Drop slash-summarize into composer"
            title="Drop /summarize into the composer; press Enter to run"
          >
            <FileText size={14} strokeWidth={1.75} aria-hidden="true" /> summary
          </button>
          <input
            ref={fileInputRef}
            type="file"
            multiple
            accept={FILE_ACCEPT}
            hidden
            onChange={(e) => {
              if (e.target.files) onPickFiles(e.target.files);
              e.target.value = "";
            }}
          />
          <button
            type="button"
            className="quick-attach-btn"
            onClick={() => fileInputRef.current?.click()}
            title="Attach images or files"
            aria-label="Attach files"
          >
            <Paperclip size={14} strokeWidth={1.75} aria-hidden="true" /> attach
          </button>
          <MicButton onTranscript={onInsert} />
        </div>
        {brainThinking && (
          <span
            className="brain-thinking"
            role="status"
            title="Local brain greping memory + recent edits for relevant @-context"
          >
            <Sparkles size={14} strokeWidth={1.75} aria-hidden="true" /> brain
            reading…
          </span>
        )}
        {attachments.length > 0 && (
          <span className="attach-preview" title={attachments.join(" ")}>
            <Paperclip size={14} strokeWidth={1.75} aria-hidden="true" />{" "}
            {attachments.length} attachment{attachments.length === 1 ? "" : "s"}{" "}
            queued
          </span>
        )}
        <button
          type="button"
          className="link-btn smart-context-trigger"
          onClick={onSuggestContext}
          disabled={sending || suggestingContext || !input.trim()}
          title="Ask AI which @-tokens to attach"
        >
          <Wand2 size={14} strokeWidth={1.75} aria-hidden="true" />
          {suggestingContext ? "thinking…" : "Suggest context"}
        </button>
        <ModelPicker />
        <ReasoningPicker />
        <button
          type="button"
          className={`link-btn compare-toggle${compareOn ? " on" : ""}`}
          onClick={onToggleCompare}
          aria-pressed={compareOn}
          title="Compare the same prompt across multiple models side-by-side"
        >
          <Columns2 size={14} strokeWidth={1.75} aria-hidden="true" /> compare
        </button>
        <button
          type="button"
          className={`link-btn enhance-toggle${enhancePrompt ? " on" : ""}`}
          onClick={onToggleEnhance}
          aria-pressed={enhancePrompt}
          title="Auto-enhance prompts with AI before sending — adds structure, specificity, and verification steps"
        >
          {enhancing ? (
            <>
              <Loader2
                size={14}
                strokeWidth={1.75}
                className="spin"
                aria-hidden="true"
              />{" "}
              enhancing…
            </>
          ) : (
            <>
              <Zap size={14} strokeWidth={1.75} aria-hidden="true" /> enhance
            </>
          )}
        </button>
        <AutoAgentButton />
      </div>
      <button
        type="button"
        className="btn-primary chat-send"
        onClick={onSend}
        disabled={!input.trim()}
        title={
          sending
            ? "Queue this message — it sends automatically when the current turn finishes"
            : "Send (Ctrl+Enter)"
        }
      >
        {sending ? "Queue" : "Send"}
      </button>
    </div>
  );
}
