import type { SandboxId, ProviderId } from "./ids.js";

/**
 * The provider-agnostic contract for a cloud (or local) sandbox: a Linux box we
 * can boot, run commands in, attach an interactive terminal to, move files in
 * and out of, and snapshot/suspend for cheap idle persistence.
 *
 * Concrete adapters (docker, e2b, fly, …) implement {@link SandboxProvider}.
 */

export type SandboxStatus =
  | "creating"
  | "running"
  | "suspended"
  | "stopped"
  | "error";

/** Resource request for a new sandbox. */
export interface SandboxResources {
  readonly cpus?: number;
  readonly memoryMb?: number;
  readonly diskMb?: number;
}

/** Everything needed to boot a box (but not yet seed a workspace). */
export interface SandboxSpec {
  /** Container/VM image with the agent runtime + MCP runtimes baked in. */
  readonly image: string;
  readonly resources?: SandboxResources;
  /** Env injected at boot. Secret values come from the control plane, not files. */
  readonly env?: Readonly<Record<string, string>>;
  /** Labels for bookkeeping (e.g. sessionId), surfaced by list(). */
  readonly labels?: Readonly<Record<string, string>>;
}

export interface Sandbox {
  readonly id: SandboxId;
  readonly providerId: ProviderId;
  readonly status: SandboxStatus;
  readonly image: string;
  readonly createdAt: string; // ISO-8601
  readonly labels: Readonly<Record<string, string>>;
}

export interface ExecOptions {
  readonly cwd?: string;
  readonly env?: Readonly<Record<string, string>>;
  /** Stream stdout/stderr lines as they arrive. */
  readonly onStdout?: (chunk: string) => void;
  readonly onStderr?: (chunk: string) => void;
  readonly timeoutMs?: number;
  /** Abort signal to cancel a long-running exec. */
  readonly signal?: AbortSignal;
}

export interface ExecResult {
  readonly exitCode: number;
  readonly stdout: string;
  readonly stderr: string;
}

/** An attached, reattachable interactive terminal stream. */
export interface PtySession {
  /** Send keystrokes / input to the terminal. */
  write(data: string): void;
  /** Terminal output, including ANSI control sequences. */
  onData(listener: (data: string) => void): void;
  /** Resize the pty (e.g. on terminal resize). */
  resize(cols: number, rows: number): void;
  /** Detach the local viewport WITHOUT killing the process in the box. */
  detach(): void;
  /** Kill the underlying process. */
  kill(): void;
  /** Resolves when the pty's process exits. */
  readonly exit: Promise<number>;
}

export interface PtyOptions {
  readonly cwd?: string;
  readonly env?: Readonly<Record<string, string>>;
  readonly cols?: number;
  readonly rows?: number;
}

export interface PutFileOptions {
  /** File mode, e.g. 0o644. */
  readonly mode?: number;
}

export interface SandboxProvider {
  readonly id: ProviderId;

  /** Boot a new box. Does not seed a workspace — that is the agent layer's job. */
  create(spec: SandboxSpec): Promise<Sandbox>;

  /** Look up a box by id, or null if it no longer exists. */
  get(id: SandboxId): Promise<Sandbox | null>;

  /** List boxes this provider knows about, optionally filtered by labels. */
  list(labels?: Readonly<Record<string, string>>): Promise<Sandbox[]>;

  /** Run a command to completion. */
  exec(id: SandboxId, command: string[], opts?: ExecOptions): Promise<ExecResult>;

  /** Attach an interactive, reattachable terminal (e.g. `claude` itself). */
  attachPty(id: SandboxId, command: string[], opts?: PtyOptions): Promise<PtySession>;

  /** Write a single file into the box (used for seeding overlays, configs). */
  putFile(id: SandboxId, path: string, content: Uint8Array, opts?: PutFileOptions): Promise<void>;

  /** Read a single file out of the box (used for reconcile, inspection). */
  getFile(id: SandboxId, path: string): Promise<Uint8Array>;

  // --- Cheap idle persistence. Not all providers support every operation; ---
  // --- those that don't should throw NotSupportedError (see errors.ts).    ---

  /** Persist box state to a restorable snapshot, returning a snapshot handle. */
  snapshot?(id: SandboxId): Promise<string>;
  /** Suspend a running box (RAM→disk) to stop paying compute while idle. */
  suspend?(id: SandboxId): Promise<void>;
  /** Resume a suspended box in place. */
  resume?(id: SandboxId): Promise<void>;

  /** Tear down a box and release its resources. */
  destroy(id: SandboxId): Promise<void>;
}
