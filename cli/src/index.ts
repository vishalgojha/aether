#!/usr/bin/env node
import { spawn } from "node:child_process";
import { Command } from "commander";
import { userInfo } from "node:os";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const program = new Command();
const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const evolutionScriptPath = resolve(repoRoot, "scripts", "evolution-loop.mjs");

program.name("aether").description("Aether operator CLI").version("0.1.0");

program
  .command("run")
  .description("Start a workflow run")
  .requiredOption("--workflow <workflow>", "workflow name (e.g. growth)")
  .option("--input <json>", "JSON input payload", "{}")
  .option("--server <url>", "aether core url", "http://127.0.0.1:8080")
  .action(async (opts) => {
    const input = safeJsonParse(opts.input);
    const response = await fetch(`${opts.server}/v1/runs`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        workflow: opts.workflow,
        input
      })
    });
    await printResponse(response);
  });

program
  .command("metrics")
  .description("Fetch raw metrics from core runtime")
  .option("--server <url>", "aether core url", "http://127.0.0.1:8080")
  .action(async (opts) => {
    const response = await fetch(`${opts.server}/metrics`);
    const body = await response.text();
    if (!response.ok) {
      throw new Error(`metrics failed: ${response.status} ${body}`);
    }
    process.stdout.write(body);
  });

program
  .command("evolve")
  .description("Run deterministic variant evolution for a workflow")
  .requiredOption("--workflow <workflow>", "workflow name (e.g. growth)")
  .option("--input <path>", "variant metrics JSON path")
  .option("--min-samples <count>", "minimum samples required for promotion")
  .option("--safety-floor <score>", "minimum safety score required for eligibility")
  .option("--dry-run", "evaluate but do not persist winner or decision log", false)
  .action(async (opts) => {
    const args = [evolutionScriptPath, "--workflow", String(opts.workflow)];
    if (opts.input) {
      args.push("--input", String(opts.input));
    }
    if (opts.minSamples) {
      args.push("--min-samples", String(opts.minSamples));
    }
    if (opts.safetyFloor) {
      args.push("--safety-floor", String(opts.safetyFloor));
    }
    if (opts.dryRun) {
      args.push("--dry-run");
    }

    const exitCode = await runSubprocess(process.execPath, args, repoRoot);
    if (exitCode !== 0) {
      throw new Error(`evolution failed with exit code ${exitCode}`);
    }
  });

program
  .command("runs")
  .description("List recent runs")
  .option("--limit <limit>", "number of runs", "20")
  .option("--server <url>", "aether core url", "http://127.0.0.1:8080")
  .action(async (opts) => {
    const limit = Number(opts.limit);
    const response = await fetch(`${opts.server}/v1/runs?limit=${encodeURIComponent(String(limit))}`);
    await printResponse(response);
  });

program
  .command("pending-approvals")
  .description("List pending approval items")
  .option("--limit <limit>", "number of approvals", "20")
  .option("--server <url>", "aether core url", "http://127.0.0.1:8080")
  .action(async (opts) => {
    const limit = Number(opts.limit);
    const response = await fetch(
      `${opts.server}/v1/approvals/pending?limit=${encodeURIComponent(String(limit))}`
    );
    await printResponse(response);
  });

program
  .command("approve")
  .description("Approve a pending high-stakes step")
  .requiredOption("--run-id <runId>", "run id")
  .requiredOption("--step-id <stepId>", "step id")
  .requiredOption("--reason <reason>", "approval rationale for audit trail")
  .option("--approver <approver>", "approver identity (defaults to env/system user)")
  .option("--actor <actor>", "approver identity (deprecated alias for --approver)")
  .option("--server <url>", "aether core url", "http://127.0.0.1:8080")
  .action(async (opts) => {
    const actor = resolveApprover(opts.approver ?? opts.actor);
    const reason = String(opts.reason ?? "").trim();
    if (!reason) {
      throw new Error("approval reason is required and cannot be empty");
    }
    const response = await fetch(`${opts.server}/v1/approve`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        run_id: opts.runId,
        step_id: opts.stepId,
        actor,
        reason
      })
    });
    await printResponse(response);
  });

program.parseAsync().catch((error: unknown) => {
  const message = error instanceof Error ? error.message : String(error);
  process.stderr.write(`aether cli error: ${message}\n`);
  process.exit(1);
});

function safeJsonParse(payload: string): unknown {
  try {
    return JSON.parse(payload);
  } catch (error) {
    throw new Error(`invalid --input JSON: ${(error as Error).message}`);
  }
}

async function printResponse(response: Response): Promise<void> {
  const body = await response.text();
  if (!response.ok) {
    throw new Error(`request failed: ${response.status} ${body}`);
  }
  process.stdout.write(`${body}\n`);
}

function resolveApprover(cliApprover: string | undefined): string {
  const fromCli = (cliApprover ?? "").trim();
  if (fromCli) {
    return fromCli;
  }

  const fromEnv = (process.env.AETHER_APPROVER ?? "").trim();
  if (fromEnv) {
    return fromEnv;
  }

  const fromUserEnv = (process.env.USERNAME ?? process.env.USER ?? "").trim();
  if (fromUserEnv) {
    return fromUserEnv;
  }

  try {
    const osUser = userInfo().username.trim();
    if (osUser) {
      return osUser;
    }
  } catch {
    // Fall through to explicit failure.
  }

  throw new Error(
    "unable to resolve approver identity; pass --approver or set AETHER_APPROVER"
  );
}

function runSubprocess(command: string, args: string[], cwd: string): Promise<number> {
  return new Promise((resolvePromise, reject) => {
    const child = spawn(command, args, {
      cwd,
      stdio: "inherit",
      shell: false
    });

    child.on("error", (error) => reject(error));
    child.on("close", (code) => resolvePromise(code ?? 1));
  });
}
