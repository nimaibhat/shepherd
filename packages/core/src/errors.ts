/** Base class for all Shepherd-specific errors. */
export class ShepherdError extends Error {
  constructor(message: string, options?: ErrorOptions) {
    super(message, options);
    this.name = new.target.name;
  }
}

/** A provider was asked for an operation it does not implement (e.g. suspend). */
export class NotSupportedError extends ShepherdError {
  constructor(providerId: string, operation: string) {
    super(`provider "${providerId}" does not support operation "${operation}"`);
  }
}

/** A referenced sandbox no longer exists. */
export class SandboxNotFoundError extends ShepherdError {
  constructor(id: string) {
    super(`sandbox not found: ${id}`);
  }
}

/** A command run inside a sandbox exited non-zero. */
export class ExecFailedError extends ShepherdError {
  constructor(
    readonly command: string[],
    readonly exitCode: number,
    readonly stderr: string,
  ) {
    super(`command failed (exit ${exitCode}): ${command.join(" ")}\n${stderr}`);
  }
}
