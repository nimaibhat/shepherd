/**
 * Branded ID types. They are strings at runtime but distinct at compile time,
 * so you cannot accidentally pass a SandboxId where a SessionId is expected.
 */
export type Brand<T, B extends string> = T & { readonly __brand: B };

export type SessionId = Brand<string, "SessionId">;
export type SandboxId = Brand<string, "SandboxId">;
export type ProviderId = string;

/** A short, URL-safe, sortable-ish id with a human-readable prefix. */
export function newId<T extends string>(prefix: string): Brand<T, string> {
  const rand = cryptoRandom(8);
  return `${prefix}_${rand}` as Brand<T, string>;
}

export const newSessionId = () => newId<SessionId>("ses");
export const newSandboxId = () => newId<SandboxId>("sbx");

function cryptoRandom(bytes: number): string {
  // Node 22 has global crypto.getRandomValues.
  const buf = new Uint8Array(bytes);
  crypto.getRandomValues(buf);
  return Array.from(buf, (b) => b.toString(16).padStart(2, "0")).join("");
}
