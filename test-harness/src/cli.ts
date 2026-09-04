import { accessSync, constants, existsSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { discoverTests } from "./yaml-loader.js";
import { runCase } from "./runner.js";
import { reportResult, reportSummary, summaryText } from "./reporter.js";

interface Args {
  candidate?: string;
  tests: string;
  filter?: string;
  verbose: boolean;
  keepFailed: boolean;
  summary?: string;
  list: boolean;
}

function parseArgs(argv: string[]): Args {
  const projectRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
  const callerCwd = process.env.SNAP_HARNESS_CALLER_CWD ?? process.cwd();
  const result: Args = { tests: resolve(projectRoot, "tests"), verbose: false, keepFailed: false, list: false };
  for (let i = 0; i < argv.length; i++) {
    switch (argv[i]) {
      case "--candidate": result.candidate = required(argv, ++i, "--candidate"); break;
      case "--tests": result.tests = resolve(callerCwd, required(argv, ++i, "--tests")); break;
      case "--filter": result.filter = required(argv, ++i, "--filter"); break;
      case "--summary": result.summary = resolve(callerCwd, required(argv, ++i, "--summary")); break;
      case "--verbose": case "-v": result.verbose = true; break;
      case "--keep-failed": result.keepFailed = true; break;
      case "--list": result.list = true; break;
      case "--help": case "-h": usage(0); break;
      default: throw new Error(`unknown argument: ${argv[i]}`);
    }
  }
  return result;
}

async function main(): Promise<void> {
  const args = parseArgs(process.argv.slice(2));
  const projectRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
  if (!existsSync(args.tests)) throw new Error(`tests directory not found: ${args.tests}`);
  const tests = discoverTests(args.tests, args.filter);
  if (tests.length === 0) throw new Error("no matching YAML tests found");
  if (args.list) {
    for (const test of tests) process.stdout.write(`${test.source}\t${test.name}\n`);
    return;
  }
  const candidate = args.candidate
    ? resolve(process.env.SNAP_HARNESS_CALLER_CWD ?? process.cwd(), args.candidate)
    : resolve(projectRoot, "run");
  try { accessSync(candidate, constants.X_OK); }
  catch { throw new Error(`candidate is not executable: ${candidate}`); }

  process.stdout.write(`snap tests — candidate=${candidate}, ${tests.length} case(s)\n`);
  const results = [];
  for (const test of tests) {
    const result = await runCase(test, { candidate, keepFailed: args.keepFailed });
    reportResult(result, args.verbose);
    results.push(result);
  }
  reportSummary(results);
  if (args.summary) writeFileSync(args.summary, summaryText(results));
  process.exitCode = results.every((result) => result.passed) ? 0 : 1;
}

function required(argv: string[], index: number, flag: string): string {
  const value = argv[index];
  if (value === undefined) throw new Error(`${flag} requires a value`);
  return value;
}

function usage(code: number): never {
  process.stdout.write(`Usage: verify [--candidate PATH] [options]\n\n` +
    `  --candidate PATH   executable to test (default: bundled run launcher)\n` +
    `  --tests PATH       alternate YAML directory\n` +
    `  --filter TEXT      filename/name substring\n` +
    `  --verbose, -v      print candidate output\n` +
    `  --keep-failed      preserve failed sandboxes\n` +
    `  --summary PATH     write plain-text summary\n` +
    `  --list             validate and list tests\n`);
  process.exit(code);
}

main().catch((error) => {
  process.stderr.write(`fatal: ${(error as Error).message}\n`);
  process.exitCode = 2;
});
