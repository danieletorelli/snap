export function parseJsonUnique(text: string): unknown {
  const value: unknown = JSON.parse(text);
  const scanner = new JsonScanner(text);
  scanner.value("$");
  scanner.space();
  if (!scanner.done()) throw new Error("unexpected trailing JSON content");
  return value;
}

class JsonScanner {
  private index = 0;
  constructor(private readonly text: string) {}

  done(): boolean { return this.index === this.text.length; }

  space(): void {
    while (/\s/.test(this.text[this.index] ?? "")) this.index++;
  }

  value(path: string): void {
    this.space();
    const c = this.text[this.index];
    if (c === "{") return this.object(path);
    if (c === "[") return this.array(path);
    if (c === '"') { this.string(); return; }
    this.primitive();
  }

  private object(path: string): void {
    this.index++;
    this.space();
    const keys = new Set<string>();
    if (this.text[this.index] === "}") { this.index++; return; }
    while (true) {
      this.space();
      const key = this.string();
      if (keys.has(key)) throw new Error(`duplicate JSON key at ${path}.${key}`);
      keys.add(key);
      this.space();
      this.expect(":");
      this.value(`${path}.${key}`);
      this.space();
      const c = this.text[this.index++];
      if (c === "}") return;
      if (c !== ",") throw new Error("invalid JSON object");
    }
  }

  private array(path: string): void {
    this.index++;
    this.space();
    if (this.text[this.index] === "]") { this.index++; return; }
    let i = 0;
    while (true) {
      this.value(`${path}[${i++}]`);
      this.space();
      const c = this.text[this.index++];
      if (c === "]") return;
      if (c !== ",") throw new Error("invalid JSON array");
    }
  }

  private string(): string {
    const start = this.index;
    this.expect('"');
    while (this.index < this.text.length) {
      const c = this.text[this.index++];
      if (c === '"') return JSON.parse(this.text.slice(start, this.index)) as string;
      if (c === "\\") {
        const escaped = this.text[this.index++];
        if (escaped === "u") this.index += 4;
      }
    }
    throw new Error("unterminated JSON string");
  }

  private primitive(): void {
    const start = this.index;
    while (this.index < this.text.length && !/[\s,\]}]/.test(this.text[this.index])) this.index++;
    if (this.index === start) throw new Error("invalid JSON value");
  }

  private expect(c: string): void {
    if (this.text[this.index] !== c) throw new Error(`expected ${c} in JSON`);
    this.index++;
  }
}
