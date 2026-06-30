import type { SandboxId, SessionId, ProviderId } from "./ids.js";
import type { WorkspaceSpec } from "./workspace.js";

/**
 * A Shepherd session: one persistent agent working in one sandbox against one
 * seeded workspace. This is the unit a user launches, detaches from, and
 * reattaches to.
 */

export type SessionStatus =
  | "pending" // created, no sandbox yet
  | "seeding" // sandbox booting + workspace seeding
  | "running" // agent actively working
  | "idle" // agent waiting; box may be suspended to save cost
  | "suspended" // box suspended to disk
  | "error"
  | "done";

export interface Session {
  readonly id: SessionId;
  readonly title: string;
  readonly status: SessionStatus;
  readonly providerId: ProviderId;
  readonly sandboxId?: SandboxId;
  readonly workspace: WorkspaceSpec;
  /** The agent's own conversation id (Claude session_id), for --resume. */
  readonly agentSessionId?: string;
  /** Branch the agent commits to for durable output + reconcile. */
  readonly branch: string;
  readonly createdAt: string; // ISO-8601
  readonly updatedAt: string; // ISO-8601
  /** Last error message, when status === "error". */
  readonly error?: string;
}

export function defaultBranchFor(id: SessionId): string {
  return `agent/${id}`;
}
