const NAME = /^[A-Za-z_][A-Za-z0-9_]*$/;

export function validateVariableName(name: string, location: string): void {
  if (!NAME.test(name)) throw new Error(`${location}: invalid variable name: ${name}`);
  if (name === "sandbox" || name === "candidate") {
    throw new Error(`${location}: cannot redefine built-in variable: ${name}`);
  }
}

export function interpolate(value: string, variables: ReadonlyMap<string, string>): string {
  let result = "";
  for (let index = 0; index < value.length;) {
    if (value.startsWith("{{{{", index)) {
      result += "{{";
      index += 4;
    } else if (value.startsWith("}}}}", index)) {
      result += "}}";
      index += 4;
    } else if (value.startsWith("{{", index)) {
      const end = value.indexOf("}}", index + 2);
      if (end < 0) throw invalidExpression(value);
      const name = value.slice(index + 2, end);
      if (!NAME.test(name)) throw invalidExpression(value);
      const replacement = variables.get(name);
      if (replacement === undefined) throw new Error(`unknown variable: ${name}`);
      result += replacement;
      index = end + 2;
    } else if (value.startsWith("}}", index)) {
      throw invalidExpression(value);
    } else {
      result += value[index];
      index++;
    }
  }
  return result;
}

function invalidExpression(value: string): Error {
  return new Error(`invalid variable expression in ${JSON.stringify(value)}`);
}

export function interpolateJson(value: unknown, variables: ReadonlyMap<string, string>): unknown {
  if (typeof value === "string") return interpolate(value, variables);
  if (Array.isArray(value)) return value.map((item) => interpolateJson(item, variables));
  if (value !== null && typeof value === "object") {
    return Object.fromEntries(
      Object.entries(value).map(([key, item]) => [key, interpolateJson(item, variables)]),
    );
  }
  return value;
}
