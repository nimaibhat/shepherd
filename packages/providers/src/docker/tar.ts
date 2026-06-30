import { pack as tarPack, extract as tarExtract } from "tar-stream";
import { Readable } from "node:stream";
import { posix as posixPath } from "node:path";

/**
 * Build a single-entry tar containing `content` at `name` (relative path).
 * Docker's putArchive extracts this under a target directory.
 */
export async function tarSingleFile(
  name: string,
  content: Uint8Array,
  mode = 0o644,
): Promise<Buffer> {
  const pack = tarPack();
  pack.entry({ name, mode }, Buffer.from(content));
  pack.finalize();
  return streamToBuffer(pack);
}

/**
 * Extract the first regular file from a tar stream (what Docker's getArchive
 * returns when fetching a single path).
 */
export async function untarFirstFile(stream: Readable): Promise<Uint8Array> {
  return new Promise<Uint8Array>((resolve, reject) => {
    const extract = tarExtract();
    let resolved = false;
    extract.on("entry", (header, entryStream, next) => {
      if (resolved || header.type !== "file") {
        entryStream.resume();
        next();
        return;
      }
      const chunks: Buffer[] = [];
      entryStream.on("data", (c: Buffer) => chunks.push(c));
      entryStream.on("end", () => {
        resolved = true;
        resolve(new Uint8Array(Buffer.concat(chunks)));
        next();
      });
      entryStream.on("error", reject);
    });
    extract.on("finish", () => {
      if (!resolved) reject(new Error("tar stream contained no file entry"));
    });
    extract.on("error", reject);
    stream.pipe(extract);
  });
}

/** Split an absolute container path into (dir, basename) for putArchive. */
export function splitContainerPath(absPath: string): { dir: string; base: string } {
  const dir = posixPath.dirname(absPath);
  const base = posixPath.basename(absPath);
  return { dir, base };
}

async function streamToBuffer(stream: Readable): Promise<Buffer> {
  const chunks: Buffer[] = [];
  for await (const chunk of stream) chunks.push(chunk as Buffer);
  return Buffer.concat(chunks);
}
