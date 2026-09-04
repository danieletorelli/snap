import {
  cpSync,
  existsSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readlinkSync,
  readdirSync,
  rmSync,
  symlinkSync,
  type Stats,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, isAbsolute, join, normalize, relative, resolve, sep } from "node:path";
import { spawnSync } from "node:child_process";
import type { EntryKind, ExpectedTreeEntry } from "./types.js";

export interface TreeEntry extends ExpectedTreeEntry {
  bytes?: Buffer;
}

export function createSandbox(): string {
  const root = mkdtempSync(join(tmpdir(), "snap-test-"));
  mkdirSync(join(root, "home"));
  mkdirSync(join(root, "tmp"));
  return root;
}

export function sandboxPath(root: string, input: string, allowRoot = true): string {
  if (input.includes("\0") || isAbsolute(input)) throw new Error(`path must be sandbox-relative: ${input}`);
  const normalized = normalize(input);
  if (normalized !== input && !(input === "." && normalized === ".")) {
    throw new Error(`path must be normalized: ${input}`);
  }
  if (normalized === ".." || normalized.startsWith(`..${sep}`)) {
    throw new Error(`path escapes sandbox: ${input}`);
  }
  if (!allowRoot && (normalized === "." || normalized === "")) {
    throw new Error("operation may not target sandbox root");
  }
  const target = resolve(root, normalized || ".");
  const rel = relative(root, target);
  if (rel === ".." || rel.startsWith(`..${sep}`) || isAbsolute(rel)) {
    throw new Error(`path escapes sandbox: ${input}`);
  }
  rejectSymlinkAncestors(root, target);
  return target;
}

function rejectSymlinkAncestors(root: string, target: string): void {
  const rel = relative(root, dirname(target));
  if (!rel || rel === ".") return;
  let current = root;
  for (const part of rel.split(sep)) {
    current = join(current, part);
    if (!existsSync(current)) break;
    if (lstatSync(current).isSymbolicLink()) throw new Error(`path traverses symlink: ${relative(root, current)}`);
  }
}

export function writeFixture(root: string, path: string, bytes: Buffer): void {
  const target = sandboxPath(root, path, false);
  if (isSymlink(target)) throw new Error(`write target is a symlink: ${path}`);
  mkdirSync(dirname(target), { recursive: true });
  writeFileSync(target, bytes);
}

export function sandboxDirectory(root: string, path: string): string {
  const target = sandboxPath(root, path);
  if (isSymlink(target)) throw new Error(`working directory is a symlink: ${path}`);
  return target;
}

export function mkdirFixture(root: string, path: string): void {
  mkdirSync(sandboxPath(root, path, false), { recursive: false });
}

export function copyTree(root: string, from: string, to: string): void {
  const source = sandboxPath(root, from, false);
  const target = sandboxPath(root, to, false);
  if (!existsSync(source)) throw new Error(`copy source does not exist: ${from}`);
  if (existsSync(target)) throw new Error(`copy destination already exists: ${to}`);
  cpSync(source, target, { recursive: true, dereference: false, verbatimSymlinks: true });
}

export function removeFixture(root: string, path: string): void {
  const target = sandboxPath(root, path, false);
  if (!existsSync(target) && !isSymlink(target)) throw new Error(`remove target does not exist: ${path}`);
  rmSync(target, { recursive: true, force: false });
}

export function symlinkFixture(root: string, path: string, target: string): void {
  const link = sandboxPath(root, path, false);
  mkdirSync(dirname(link), { recursive: true });
  symlinkSync(target, link);
}

export function fifoFixture(root: string, path: string): void {
  const target = sandboxPath(root, path, false);
  mkdirSync(dirname(target), { recursive: true });
  const result = spawnSync("mkfifo", [target], { encoding: "utf8" });
  if (result.status !== 0) throw new Error(`mkfifo failed: ${result.stderr.trim()}`);
}

export function listTree(root: string, path: string, ignore: string[] = []): TreeEntry[] {
  const base = sandboxPath(root, path);
  if (!existsSync(base)) throw new Error(`tree root does not exist: ${path}`);
  if (!lstatSync(base).isDirectory()) throw new Error(`tree root is not a directory: ${path}`);
  const entries: TreeEntry[] = [];
  walk(base, "", entries, ignore);
  entries.sort((a, b) => Buffer.compare(Buffer.from(a.path), Buffer.from(b.path)));
  return entries;
}

function walk(base: string, rel: string, entries: TreeEntry[], ignore: string[]): void {
  for (const name of readdirSync(join(base, rel))) {
    const childRel = rel ? `${rel}/${name}` : name;
    if (ignored(childRel, ignore)) continue;
    const full = join(base, childRel);
    const stat = lstatSync(full);
    const kind = kindOf(stat);
    const entry: TreeEntry = { path: childRel, kind };
    if (kind === "symlink") entry.target = readlinkSync(full);
    if (kind === "file") entry.bytes = readFileSync(full);
    entries.push(entry);
    if (kind === "directory") walk(base, childRel, entries, ignore);
  }
}

function ignored(path: string, prefixes: string[]): boolean {
  return prefixes.some((prefix) => path === prefix || path.startsWith(`${prefix}/`));
}

function kindOf(stat: Stats): EntryKind {
  if (stat.isFile()) return "file";
  if (stat.isDirectory()) return "directory";
  if (stat.isSymbolicLink()) return "symlink";
  if (stat.isFIFO()) return "fifo";
  return "other";
}

function isSymlink(path: string): boolean {
  try { return lstatSync(path).isSymbolicLink(); } catch { return false; }
}
