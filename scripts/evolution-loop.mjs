#!/usr/bin/env node
/**
 * Deterministic, local-first variant evolution loop.
 * - Reads variant metrics from JSON
 * - Selects a winner by deterministic scoring + tie-breakers
 * - Applies a minimum sample gate before promotion
 * - Persists active variant and appends JSONL decision audit
 * - Emits metric-like lines for local observability
 */

import { appendFile, mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const DEFAULT_STATE_DIR = "state";
const DEFAULT_INPUT = path.join(DEFAULT_STATE_DIR, "variant-metrics.json");
const DEFAULT_WORKFLOW = "growth";
const DEFAULT_MIN_SAMPLES = 5;
const DEFAULT_SAFETY_FLOOR = Number(process.env.AETHER_SAFETY_FLOOR ?? "0.95");

export async function runEvolution(input) {
  const opts = normalizeOptions(input);
  const rawVariants = await readVariants(opts.inputPath);
  const prepared = normalizeVariants(rawVariants, opts.workflow);
  if (prepared.length === 0) {
    throw new Error(`No variants found for workflow '${opts.workflow}'.`);
  }

  const eligible = prepared.filter((variant) => variant.safetyScore >= opts.safetyFloor);
  if (eligible.length === 0) {
    throw new Error("No eligible variants pass safety floor.");
  }

  const scored = eligible.map((variant) => ({
    ...variant,
    score: scoreVariant(variant)
  }));
  scored.sort(compareVariants);

  const winner = scored[0];
  const promoted = winner.sampleSize >= opts.minSamples;
  const status = promoted ? "promoted" : "insufficient_samples";
  const selectedAt = opts.now.toISOString();

  for (const variant of scored) {
    emitMetricLine("variant_score", {
      workflow: opts.workflow,
      variant_id: variant.variantId,
      value: formatMetricNumber(variant.score)
    });
  }

  emitMetricLine("variant_selection_total", {
    workflow: opts.workflow,
    status,
    variant_id: winner.variantId,
    value: "1"
  });

  const decision = {
    timestamp: selectedAt,
    workflow: opts.workflow,
    status,
    promoted_variant: promoted ? winner.variantId : null,
    winner: summarizeVariant(winner),
    min_samples: opts.minSamples,
    safety_floor: opts.safetyFloor,
    candidates: scored.map(summarizeVariant)
  };

  if (!opts.dryRun) {
    await mkdir(opts.stateDir, { recursive: true });
    await appendDecisionLog(opts.decisionsPath, decision);
    if (promoted) {
      await writeActiveVariant(opts.activeVariantPath, opts.workflow, winner, selectedAt);
    }
  }

  return decision;
}

export function scoreVariant(variant) {
  const baseScore = variant.successRate * 100;
  const sampleBonus = Math.min(variant.sampleSize / 10, 20);
  const costPenalty = variant.avgCostPerRunUsd * 0.5;
  const latencyPenalty = variant.p95LatencyMs / 10_000;
  return baseScore + sampleBonus - costPenalty - latencyPenalty;
}

export function compareVariants(left, right) {
  if (right.score !== left.score) {
    return right.score - left.score;
  }
  if (left.avgCostPerRunUsd !== right.avgCostPerRunUsd) {
    return left.avgCostPerRunUsd - right.avgCostPerRunUsd;
  }
  if (right.sampleSize !== left.sampleSize) {
    return right.sampleSize - left.sampleSize;
  }
  return left.variantId.localeCompare(right.variantId);
}

function summarizeVariant(variant) {
  return {
    variant_id: variant.variantId,
    score: Number(variant.score.toFixed(6)),
    success_rate: variant.successRate,
    sample_size: variant.sampleSize,
    avg_cost_per_run_usd: variant.avgCostPerRunUsd,
    p95_latency_ms: variant.p95LatencyMs,
    safety_score: variant.safetyScore
  };
}

async function readVariants(inputPath) {
  const raw = await readFile(inputPath, "utf-8");
  const parsed = JSON.parse(raw);
  if (!Array.isArray(parsed)) {
    throw new Error("Variant metrics input must be a JSON array.");
  }
  return parsed;
}

function normalizeVariants(rawVariants, workflow) {
  return rawVariants
    .map((raw) => normalizeVariant(raw, workflow))
    .filter((variant) => variant.workflow === workflow);
}

function normalizeVariant(raw, defaultWorkflow) {
  const workflow = normalizeWorkflow(raw.workflow ?? raw.workflow_name ?? raw.flow ?? defaultWorkflow);
  const variantId = trimString(raw.variant_id ?? raw.variant ?? raw.id ?? "unknown");
  return {
    workflow,
    variantId,
    successRate: normalizeNumber(raw.success_rate ?? raw.successRate ?? raw.win_rate ?? 0),
    sampleSize: normalizeNumber(raw.sample_size ?? raw.sampleSize ?? raw.samples ?? raw.runs ?? 0),
    avgCostPerRunUsd: normalizeNumber(
      raw.avg_cost_per_run_usd ??
        raw.avgCostPerRun ??
        raw.cost_per_success_usd ??
        raw.costPerSuccessUsd ??
        0
    ),
    p95LatencyMs: normalizeNumber(raw.p95_latency_ms ?? raw.p95LatencyMs ?? raw.latency_p95_ms ?? 0),
    safetyScore: normalizeNumber(raw.safety_score ?? raw.safetyScore ?? 1)
  };
}

function normalizeOptions(input) {
  const options = input ?? {};
  const stateDir = trimString(options.stateDir ?? DEFAULT_STATE_DIR);
  return {
    workflow: normalizeWorkflow(options.workflow ?? DEFAULT_WORKFLOW),
    inputPath: trimString(options.inputPath ?? DEFAULT_INPUT),
    stateDir,
    activeVariantPath: trimString(
      options.activeVariantPath ?? path.join(stateDir, "active-variant.json")
    ),
    decisionsPath: trimString(
      options.decisionsPath ?? path.join(stateDir, "variant-decisions.jsonl")
    ),
    minSamples: Math.max(0, Math.floor(normalizeNumber(options.minSamples ?? DEFAULT_MIN_SAMPLES))),
    safetyFloor: normalizeNumber(options.safetyFloor ?? DEFAULT_SAFETY_FLOOR),
    dryRun: Boolean(options.dryRun),
    now: options.now instanceof Date ? options.now : new Date()
  };
}

async function writeActiveVariant(activeVariantPath, workflow, winner, selectedAt) {
  const active = await readJsonOrDefault(activeVariantPath, { workflows: {} });
  if (!active.workflows || typeof active.workflows !== "object") {
    active.workflows = {};
  }

  active.updated_at = selectedAt;
  active.workflows[workflow] = {
    workflow,
    variant_id: winner.variantId,
    score: Number(winner.score.toFixed(6)),
    success_rate: winner.successRate,
    sample_size: winner.sampleSize,
    avg_cost_per_run_usd: winner.avgCostPerRunUsd,
    p95_latency_ms: winner.p95LatencyMs,
    safety_score: winner.safetyScore,
    selected_at: selectedAt
  };

  await writeFile(activeVariantPath, `${JSON.stringify(active, null, 2)}\n`, "utf-8");
}

async function appendDecisionLog(decisionsPath, decision) {
  await appendFile(decisionsPath, `${JSON.stringify(decision)}\n`, "utf-8");
}

function emitMetricLine(name, labels) {
  const fragments = Object.entries(labels).map(([key, value]) => `${key}=${value}`);
  process.stdout.write(`METRIC ${name} ${fragments.join(" ")}\n`);
}

function formatMetricNumber(value) {
  return Number(value).toFixed(6);
}

async function readJsonOrDefault(targetPath, fallbackValue) {
  try {
    const raw = await readFile(targetPath, "utf-8");
    return JSON.parse(raw);
  } catch {
    return fallbackValue;
  }
}

function normalizeNumber(value) {
  const normalized = Number(value);
  if (!Number.isFinite(normalized)) {
    return 0;
  }
  return normalized;
}

function trimString(value) {
  return String(value ?? "").trim();
}

function normalizeWorkflow(value) {
  return trimString(value).toLowerCase();
}

function parseCliArgs(argv) {
  const options = {};
  const positional = [];
  for (let i = 0; i < argv.length; i += 1) {
    const token = argv[i];
    switch (token) {
      case "--workflow":
        options.workflow = argv[++i];
        break;
      case "--input":
      case "--metrics-path":
        options.inputPath = argv[++i];
        break;
      case "--state-dir":
        options.stateDir = argv[++i];
        break;
      case "--min-samples":
        options.minSamples = argv[++i];
        break;
      case "--safety-floor":
        options.safetyFloor = argv[++i];
        break;
      case "--dry-run":
        options.dryRun = true;
        break;
      default:
        if (token.startsWith("-")) {
          throw new Error(`Unknown option: ${token}`);
        }
        positional.push(token);
        break;
    }
  }

  if (positional.length > 0) {
    const first = positional[0];
    if (first.toLowerCase().endsWith(".json") && !options.inputPath) {
      options.inputPath = first;
    } else if (!options.workflow) {
      options.workflow = first;
    } else if (!options.inputPath) {
      options.inputPath = first;
    }
  }

  return options;
}

async function main() {
  const options = parseCliArgs(process.argv.slice(2));
  const decision = await runEvolution(options);
  process.stdout.write(`${JSON.stringify(decision, null, 2)}\n`);
}

function isDirectExecution(metaUrl) {
  if (!process.argv[1]) {
    return false;
  }
  return pathToFileURL(path.resolve(process.argv[1])).href === metaUrl;
}

if (isDirectExecution(import.meta.url)) {
  main().catch((error) => {
    const message = error instanceof Error ? error.message : String(error);
    process.stderr.write(`evolution loop error: ${message}\n`);
    process.exit(1);
  });
}
