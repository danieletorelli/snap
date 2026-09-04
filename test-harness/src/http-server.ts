import { createServer, request as httpRequest, type Server } from "node:http";
import { connect } from "node:net";
import type { HttpResult, HttpRoute, ExpectedHttpRequest } from "./types.js";

const BODY_LIMIT = 16 * 1024 * 1024;

export interface ControlledServer {
  server: Server;
  url: string;
  requests: ExpectedHttpRequest[];
}

export async function startControlledServer(routes: HttpRoute[]): Promise<ControlledServer> {
  const requests: ExpectedHttpRequest[] = [];
  const server = createServer((request, response) => {
    const method = request.method ?? "GET";
    const target = request.url ?? "/";
    requests.push({ method, target });
    const route = routes.find((candidate) => candidate.method === method && candidate.target === target);
    if (!route) { response.statusCode = 404; response.end(); return; }
    response.statusCode = route.status;
    for (const [name, value] of Object.entries(route.headers ?? {})) response.setHeader(name, value);
    const body = route.text !== undefined ? Buffer.from(route.text) : Buffer.from(route.base64 ?? "", "base64");
    response.end(body);
  });
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => resolve());
  });
  const address = server.address();
  if (address === null || typeof address === "string") throw new Error("controlled HTTP server has no TCP address");
  return { server, url: `http://127.0.0.1:${address.port}`, requests };
}

export async function stopControlledServer(server: ControlledServer): Promise<void> {
  await new Promise<void>((resolve, reject) => server.server.close((error) => error ? reject(error) : resolve()));
}

export async function performHttpRequest(
  method: string,
  urlText: string,
  headers: Record<string, string>,
  timeoutMs: number,
): Promise<HttpResult> {
  const url = new URL(urlText);
  if (url.protocol !== "http:") throw new Error(`public harness HTTP client only supports http://, got ${url.protocol}`);
  if (method === "HEAD") return rawHead(url, headers, timeoutMs);
  return normalRequest(method, url, headers, timeoutMs);
}

function normalRequest(
  method: string,
  url: URL,
  headers: Record<string, string>,
  timeoutMs: number,
): Promise<HttpResult> {
  return new Promise((resolve, reject) => {
    const request = httpRequest(url, { method, headers }, (response) => {
      const chunks: Buffer[] = [];
      let size = 0;
      response.on("data", (chunk: Buffer) => {
        size += chunk.length;
        if (size > BODY_LIMIT) {
          request.destroy(new Error("HTTP body exceeded 16 MiB limit"));
          return;
        }
        chunks.push(chunk);
      });
      response.on("end", () => resolve({
        status: response.statusCode ?? 0,
        headers: normalizeHeaders(response.headers),
        body: Buffer.concat(chunks),
      }));
    });
    request.once("error", reject);
    request.setTimeout(timeoutMs, () => request.destroy(new Error(`HTTP request timed out after ${timeoutMs}ms`)));
    request.end();
  });
}

function rawHead(url: URL, headers: Record<string, string>, timeoutMs: number): Promise<HttpResult> {
  return new Promise((resolve, reject) => {
    const port = Number(url.port || 80);
    const socket = connect(port, url.hostname);
    const chunks: Buffer[] = [];
    let size = 0;
    socket.setTimeout(timeoutMs, () => socket.destroy(new Error(`HTTP request timed out after ${timeoutMs}ms`)));
    socket.once("error", reject);
    socket.on("data", (chunk) => {
      size += chunk.length;
      if (size > BODY_LIMIT) { socket.destroy(new Error("HTTP response exceeded 16 MiB limit")); return; }
      chunks.push(chunk);
    });
    socket.once("connect", () => {
      const target = `${url.pathname}${url.search}` || "/";
      const lines = [`HEAD ${target} HTTP/1.1`, `Host: ${url.host}`, "Connection: close"];
      for (const [name, value] of Object.entries(headers)) lines.push(`${name}: ${value}`);
      socket.write(`${lines.join("\r\n")}\r\n\r\n`);
    });
    socket.once("close", (hadError) => {
      if (hadError) return;
      try { resolve(parseRawResponse(Buffer.concat(chunks))); } catch (error) { reject(error); }
    });
  });
}

function parseRawResponse(raw: Buffer): HttpResult {
  const separator = raw.indexOf("\r\n\r\n");
  if (separator < 0) throw new Error("malformed HTTP response: missing header terminator");
  const headerText = new TextDecoder("utf-8", { fatal: true }).decode(raw.subarray(0, separator));
  const lines = headerText.split("\r\n");
  const statusMatch = lines.shift()?.match(/^HTTP\/1\.[01] (\d{3})(?: |$)/);
  if (!statusMatch) throw new Error("malformed HTTP status line");
  const headers: Record<string, string> = {};
  for (const line of lines) {
    const colon = line.indexOf(":");
    if (colon <= 0) throw new Error(`malformed HTTP header: ${line}`);
    const name = line.slice(0, colon).toLowerCase();
    const value = line.slice(colon + 1).trim();
    headers[name] = headers[name] === undefined ? value : `${headers[name]}, ${value}`;
  }
  return { status: Number(statusMatch[1]), headers, body: raw.subarray(separator + 4) };
}

function normalizeHeaders(headers: NodeJS.Dict<string | string[]>): Record<string, string> {
  return Object.fromEntries(
    Object.entries(headers).flatMap(([name, value]) => value === undefined ? [] : [[name.toLowerCase(), Array.isArray(value) ? value.join(", ") : value]]),
  );
}
