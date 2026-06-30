/**
 * How a sandbox's workspace is seeded.
 *
 * Git is the transport, not a network mount. The canonical workspace lives in
 * the sandbox after seeding; we never live-mirror the user's local filesystem
 * (see PLAN.md §2). Seeding is a one-time, point-in-time operation.
 */

/** A captured snapshot of uncommitted local work, applied on top of a clone. */
export interface DirtyOverlay {
  /** Output of `git diff HEAD` (tracked changes), empty string if none. */
  readonly diff: string;
  /**
   * Untracked-but-not-ignored files, as a base64 tar (gzip). Applied after the
   * diff so the agent starts from exactly what the user sees locally.
   */
  readonly untrackedTarGz?: string;
  /** The commit the diff/overlay was captured against, for sanity checks. */
  readonly baseCommit: string;
}

/** Seed from a git remote — the primary, preferred path. */
export interface GitWorkspaceSpec {
  readonly kind: "git";
  readonly repoUrl: string;
  /** Branch, tag, or commit to check out. Defaults to the remote HEAD. */
  readonly ref?: string;
  /** Shallow clone depth. Omit for a full clone. */
  readonly depth?: number;
  /** Optional uncommitted local state to overlay on top of the checkout. */
  readonly dirtyOverlay?: DirtyOverlay;
  /** Where to place the checkout inside the sandbox. Default: /workspace. */
  readonly mountPath?: string;
}

/** Seed from an uploaded archive — for non-git folders (degenerate case). */
export interface ArchiveWorkspaceSpec {
  readonly kind: "archive";
  /** base64 tar (gzip) of the directory tree. */
  readonly tarGz: string;
  readonly mountPath?: string;
}

export type WorkspaceSpec = GitWorkspaceSpec | ArchiveWorkspaceSpec;

export const DEFAULT_MOUNT_PATH = "/workspace";

export function mountPathOf(spec: WorkspaceSpec): string {
  return spec.mountPath ?? DEFAULT_MOUNT_PATH;
}
