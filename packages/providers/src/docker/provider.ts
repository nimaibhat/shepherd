import Docker from "dockerode";
import { PassThrough } from "node:stream";
import {
  type SandboxProvider,
  type Sandbox,
  type SandboxSpec,
  type SandboxStatus,
  type SandboxId,
  type ExecOptions,
  type ExecResult,
  type PtySession,
  type PtyOptions,
  type PutFileOptions,
  SandboxNotFoundError,
  newSandboxId,
} from "@shepherd/core";
import { tarSingleFile, untarFirstFile, splitContainerPath } from "./tar.js";

const MANAGED_LABEL = "shepherd.managed";
const ID_LABEL = "shepherd.id";

export interface DockerProviderOptions {
  /** Passed through to dockerode (socketPath, host, port, …). Default: local. */
  readonly docker?: ConstructorParameters<typeof Docker>[0];
  /** Keep-alive command for the box. Default: sleep infinity. */
  readonly keepAliveCmd?: string[];
}

/**
 * SandboxProvider backed by local Docker. Lets the entire Shepherd flow run with
 * zero cloud cost or accounts (PLAN.md §7). Maps idle-persistence onto Docker:
 * suspend→pause, resume→unpause, snapshot→commit.
 */
export class DockerProvider implements SandboxProvider {
  readonly id = "docker";
  private readonly docker: Docker;
  private readonly keepAliveCmd: string[];

  constructor(opts: DockerProviderOptions = {}) {
    this.docker = new Docker(opts.docker);
    this.keepAliveCmd = opts.keepAliveCmd ?? ["sleep", "infinity"];
  }

  async create(spec: SandboxSpec): Promise<Sandbox> {
    await this.ensureImage(spec.image);
    const id = newSandboxId();
    const container = await this.docker.createContainer({
      Image: spec.image,
      name: `shepherd-${id}`,
      Cmd: this.keepAliveCmd,
      Tty: false,
      Labels: { ...spec.labels, [MANAGED_LABEL]: "true", [ID_LABEL]: id },
      Env: toEnvArray(spec.env),
      HostConfig: hostConfig(spec),
    });
    await container.start();
    const sandbox = await this.inspectToSandbox(container, id);
    if (!sandbox) throw new SandboxNotFoundError(id);
    return sandbox;
  }

  async get(id: SandboxId): Promise<Sandbox | null> {
    const container = await this.find(id);
    if (!container) return null;
    return this.inspectToSandbox(container, id);
  }

  async list(labels?: Readonly<Record<string, string>>): Promise<Sandbox[]> {
    const labelFilters = [`${MANAGED_LABEL}=true`, ...labelPairs(labels)];
    const infos = await this.docker.listContainers({
      all: true,
      filters: { label: labelFilters },
    });
    const out: Sandbox[] = [];
    for (const info of infos) {
      const id = (info.Labels?.[ID_LABEL] ?? "") as SandboxId;
      out.push(toSandbox(id, info.Image, mapState(info.State), info.Created * 1000, info.Labels ?? {}));
    }
    return out;
  }

  async exec(id: SandboxId, command: string[], opts: ExecOptions = {}): Promise<ExecResult> {
    const container = await this.requireContainer(id);
    const exec = await container.exec({
      Cmd: command,
      AttachStdout: true,
      AttachStderr: true,
      WorkingDir: opts.cwd,
      Env: toEnvArray(opts.env),
    });
    const stream = await exec.start({ hijack: true, stdin: false });

    const stdoutPT = new PassThrough();
    const stderrPT = new PassThrough();
    this.docker.modem.demuxStream(stream, stdoutPT, stderrPT);

    let stdout = "";
    let stderr = "";
    stdoutPT.on("data", (c: Buffer) => {
      const s = c.toString("utf8");
      stdout += s;
      opts.onStdout?.(s);
    });
    stderrPT.on("data", (c: Buffer) => {
      const s = c.toString("utf8");
      stderr += s;
      opts.onStderr?.(s);
    });

    const onAbort = () => stream.destroy();
    opts.signal?.addEventListener("abort", onAbort, { once: true });

    await new Promise<void>((resolve, reject) => {
      stream.on("end", resolve);
      stream.on("error", reject);
    }).finally(() => {
      opts.signal?.removeEventListener("abort", onAbort);
      stdoutPT.end();
      stderrPT.end();
    });

    const info = await exec.inspect();
    return { exitCode: info.ExitCode ?? -1, stdout, stderr };
  }

  async attachPty(id: SandboxId, command: string[], opts: PtyOptions = {}): Promise<PtySession> {
    const container = await this.requireContainer(id);
    const exec = await container.exec({
      Cmd: command,
      AttachStdin: true,
      AttachStdout: true,
      AttachStderr: true,
      Tty: true,
      WorkingDir: opts.cwd,
      Env: toEnvArray(opts.env),
    });
    const stream = await exec.start({ hijack: true, stdin: true });
    if (opts.cols && opts.rows) {
      await exec.resize({ w: opts.cols, h: opts.rows }).catch(() => {});
    }

    let exitResolve!: (code: number) => void;
    const exit = new Promise<number>((res) => (exitResolve = res));
    stream.on("end", async () => {
      const info = await exec.inspect().catch(() => ({ ExitCode: -1 }));
      exitResolve(info.ExitCode ?? -1);
    });

    return {
      write: (data) => stream.write(data),
      onData: (listener) => stream.on("data", (c: Buffer) => listener(c.toString("utf8"))),
      resize: (cols, rows) => {
        void exec.resize({ w: cols, h: rows }).catch(() => {});
      },
      // For the Docker dev provider, detaching the local stream and killing are
      // currently the same operation; true survive-detach reattach is M6 (the
      // control daemon owns the long-lived pty, clients attach over websocket).
      detach: () => stream.destroy(),
      kill: () => stream.destroy(),
      exit,
    };
  }

  async putFile(id: SandboxId, path: string, content: Uint8Array, opts: PutFileOptions = {}): Promise<void> {
    const container = await this.requireContainer(id);
    const { dir, base } = splitContainerPath(path);
    const archive = await tarSingleFile(base, content, opts.mode ?? 0o644);
    await container.putArchive(archive, { path: dir });
  }

  async getFile(id: SandboxId, path: string): Promise<Uint8Array> {
    const container = await this.requireContainer(id);
    const stream = await container.getArchive({ path });
    return untarFirstFile(stream as unknown as NodeJS.ReadableStream as never);
  }

  async snapshot(id: SandboxId): Promise<string> {
    const container = await this.requireContainer(id);
    const res = await container.commit({ repo: "shepherd-snapshot", tag: id });
    return res.Id;
  }

  async suspend(id: SandboxId): Promise<void> {
    const container = await this.requireContainer(id);
    await container.pause();
  }

  async resume(id: SandboxId): Promise<void> {
    const container = await this.requireContainer(id);
    await container.unpause();
  }

  async destroy(id: SandboxId): Promise<void> {
    const container = await this.find(id);
    if (!container) return;
    await container.remove({ force: true });
  }

  // --- internals ---

  private async ensureImage(image: string): Promise<void> {
    try {
      await this.docker.getImage(image).inspect();
      return;
    } catch {
      // not present locally → pull
    }
    const stream = await this.docker.pull(image);
    await new Promise<void>((resolve, reject) => {
      this.docker.modem.followProgress(stream, (err) => (err ? reject(err) : resolve()));
    });
  }

  private async find(id: SandboxId): Promise<Docker.Container | null> {
    const infos = await this.docker.listContainers({
      all: true,
      filters: { label: [`${ID_LABEL}=${id}`] },
    });
    const info = infos[0];
    if (!info) return null;
    return this.docker.getContainer(info.Id);
  }

  private async requireContainer(id: SandboxId): Promise<Docker.Container> {
    const container = await this.find(id);
    if (!container) throw new SandboxNotFoundError(id);
    return container;
  }

  private async inspectToSandbox(container: Docker.Container, id: SandboxId): Promise<Sandbox | null> {
    try {
      const info = await container.inspect();
      const created = Date.parse(info.Created);
      return toSandbox(id, info.Config.Image, mapInspectState(info.State), created, info.Config.Labels ?? {});
    } catch {
      return null;
    }
  }
}

function toSandbox(
  id: SandboxId,
  image: string,
  status: SandboxStatus,
  createdMs: number,
  labels: Record<string, string>,
): Sandbox {
  return {
    id,
    providerId: "docker",
    status,
    image,
    createdAt: new Date(createdMs).toISOString(),
    labels,
  };
}

function hostConfig(spec: SandboxSpec): Docker.ContainerCreateOptions["HostConfig"] {
  const hc: NonNullable<Docker.ContainerCreateOptions["HostConfig"]> = {};
  const r = spec.resources;
  if (r?.memoryMb) hc.Memory = r.memoryMb * 1024 * 1024;
  if (r?.cpus) hc.NanoCpus = Math.round(r.cpus * 1e9);
  return hc;
}

function toEnvArray(env?: Readonly<Record<string, string>>): string[] | undefined {
  if (!env) return undefined;
  return Object.entries(env).map(([k, v]) => `${k}=${v}`);
}

function labelPairs(labels?: Readonly<Record<string, string>>): string[] {
  if (!labels) return [];
  return Object.entries(labels).map(([k, v]) => `${k}=${v}`);
}

/** Map `docker ps` State string to our status. */
function mapState(state: string): SandboxStatus {
  switch (state) {
    case "running":
      return "running";
    case "paused":
      return "suspended";
    case "created":
      return "creating";
    case "exited":
    case "dead":
    case "removing":
      return "stopped";
    default:
      return "error";
  }
}

/** Map a full inspect State object to our status. */
function mapInspectState(state: Docker.ContainerInspectInfo["State"]): SandboxStatus {
  if (state.Paused) return "suspended";
  if (state.Running) return "running";
  if (state.Status === "created") return "creating";
  if (state.Dead || state.Status === "exited") return "stopped";
  return "error";
}
