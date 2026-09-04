import type { TestResult } from "./types.js";

const RED = "\x1b[31m";
const GREEN = "\x1b[32m";
const DIM = "\x1b[2m";
const BOLD = "\x1b[1m";
const RESET = "\x1b[0m";
const noColor = process.env.NO_COLOR !== undefined;
const color = (code: string, text: string) => noColor ? text : `${code}${text}${RESET}`;

export function reportResult(result: TestResult, verbose: boolean): void {
  const icon = result.passed ? color(GREEN, "✓") : color(RED, "✗");
  process.stdout.write(`  ${icon} ${result.name} ${color(DIM, `${result.durationMs}ms`)}\n`);
  if (!result.passed) {
    if (result.error && !result.steps.some((step) => step.failures.includes(result.error!))) {
      printFailure(result.error);
    }
    for (const step of result.steps) {
      for (const failure of step.failures) printFailure(`step ${step.index + 1} (${step.label}): ${failure}`);
    }
    if (result.sandbox) process.stdout.write(`      sandbox: ${result.sandbox}\n`);
  }
  if (verbose) {
    for (const step of result.steps) {
      if (!step.process) continue;
      process.stdout.write(color(DIM, `      [step ${step.index + 1} stdout]\n${indent(step.process.stdout)}`));
      process.stdout.write(color(DIM, `      [step ${step.index + 1} stderr]\n${indent(step.process.stderr)}`));
      process.stdout.write(color(DIM, `      [exit ${step.process.exitCode}${step.process.signal ? ` ${step.process.signal}` : ""}]\n`));
    }
  }
}

export function reportSummary(results: TestResult[]): void {
  const passed = results.filter((result) => result.passed).length;
  const failed = results.length - passed;
  const duration = results.reduce((sum, result) => sum + result.durationMs, 0);
  process.stdout.write("\n");
  if (failed === 0) process.stdout.write(color(BOLD, color(GREEN, `${passed} passed`)) + color(DIM, ` in ${duration}ms\n`));
  else process.stdout.write(color(BOLD, color(RED, `${failed} failed`)) + `, ${color(GREEN, `${passed} passed`)}` + color(DIM, ` in ${duration}ms\n`));
}

export function summaryText(results: TestResult[]): string {
  const lines = results.map((result) => result.passed
    ? `PASS: ${result.name} (${result.durationMs}ms)`
    : `FAIL: ${result.name} (${result.durationMs}ms) — ${result.steps.flatMap((step) => step.failures).join("; ") || result.error}`);
  lines.push(`SUMMARY: ${results.filter((result) => result.passed).length}/${results.length} passed`);
  return `${lines.join("\n")}\n`;
}

function printFailure(message: string): void {
  for (const line of message.split("\n")) process.stdout.write(`      ${color(RED, line)}\n`);
}

function indent(text: string): string {
  return `${text.split("\n").map((line) => `      | ${line}`).join("\n")}\n`;
}
