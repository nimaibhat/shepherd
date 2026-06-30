import type { SandboxId } from "./ids.js";
import type { SandboxProvider } from "./sandbox.js";

/**
 * Drives a headless coding agent inside a sandbox. We do NOT reimplement the
 * agent loop — for Claude Code this shells out to `claude -p ... --resume`
 * (see PLAN.md §6). Other CLIs are a different runner, not a rewrite.
 */

export type AgentEvent =
  | { readonly type: "session"; readonly agentSessionId: string }
  | { readonly type: "text"; readonly text: string }
  | { readonly type: "tool_use"; readonly name: string; readonly input: unknown }
  | { readonly type: "tool_result"; readonly name: string; readonly ok: boolean }
  | { readonly type: "error"; readonly message: string }
  | { readonly type: "done"; readonly exitCode: number };

export interface RunRequest {
  readonly sandboxId: SandboxId;
  readonly prompt: string;
  /** Working directory inside the box (the seeded workspace). */
  readonly cwd: string;
  /**
   * Resume an existing agent conversation by its id (Claude's session_id).
   * Omit to start a fresh conversation.
   */
  readonly resumeAgentSessionId?: string;
  /** Allowed tools, passed through to the agent CLI. */
  readonly allowedTools?: readonly string[];
  readonly env?: Readonly<Record<string, string>>;
  readonly signal?: AbortSignal;
}

export interface RunResult {
  /** The agent's own session id, captured for later --resume. */
  readonly agentSessionId: string;
  readonly exitCode: number;
}

export interface AgentRunner {
  /** Human-readable runner name, e.g. "claude-code". */
  readonly name: string;
  /**
   * Run one turn (or a whole task) non-interactively, streaming events.
   * Resolves when the agent process exits.
   */
  run(
    provider: SandboxProvider,
    req: RunRequest,
    onEvent: (e: AgentEvent) => void,
  ): Promise<RunResult>;
}
