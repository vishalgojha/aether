#!/usr/bin/env node
import { access, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import assert from "node:assert/strict";

import { compareVariants, runEvolution, scoreVariant } from "./evolution-loop.mjs";

async function main() {
  await testCompareVariantsTieBreakers();
  await testScoreVariantMonotonicity();
  await testPromotionPersistsActiveAndDecision();
  await testSampleGateBlocksPromotion();
  await testObservationAggregationAndLatestRunSelection();
  process.stdout.write("evolution tests passed\n");
}

async function testCompareVariantsTieBreakers() {
  const left = {
    variantId: "variant-b",
    score: 80,
    avgCostPerRunUsd: 2.0,
    sampleSize: 8
  };
  const right = {
    variantId: "variant-a",
    score: 80,
    avgCostPerRunUsd: 1.5,
    sampleSize: 8
  };
  assert.equal(compareVariants(left, right) > 0, true);
  assert.equal(compareVariants(right, left) < 0, true);
}

async function testScoreVariantMonotonicity() {
  const low = scoreVariant({
    successRate: 0.7,
    sampleSize: 5,
    avgCostPerRunUsd: 8,
    p95LatencyMs: 5000
  });
  const high = scoreVariant({
    successRate: 0.8,
    sampleSize: 20,
    avgCostPerRunUsd: 4,
    p95LatencyMs: 1200
  });
  assert.equal(high > low, true);
}

async function testPromotionPersistsActiveAndDecision() {
  const stateDir = await mkdtemp(path.join(tmpdir(), "aether-evolve-promote-"));
  const inputPath = path.join(stateDir, "variant-metrics.json");
  await writeFile(
    inputPath,
    JSON.stringify(
      [
        {
          workflow: "growth",
          variant: "v1",
          success_rate: 0.74,
          sample_size: 12,
          avg_cost_per_run_usd: 6,
          p95_latency_ms: 3000,
          safety_score: 0.98
        },
        {
          workflow: "growth",
          variant: "v2",
          success_rate: 0.81,
          sample_size: 15,
          avg_cost_per_run_usd: 5,
          p95_latency_ms: 1900,
          safety_score: 0.99
        }
      ],
      null,
      2
    ),
    "utf-8"
  );

  const decision = await runEvolution({
    workflow: "growth",
    inputPath,
    stateDir,
    minSamples: 5,
    safetyFloor: 0.95,
    now: new Date("2026-03-05T10:00:00.000Z")
  });

  assert.equal(decision.status, "promoted");
  assert.equal(decision.promoted_variant, "v2");

  const activeRaw = await readFile(path.join(stateDir, "active-variant.json"), "utf-8");
  const active = JSON.parse(activeRaw);
  assert.equal(active.workflows.growth.variant_id, "v2");

  const decisionsRaw = await readFile(path.join(stateDir, "variant-decisions.jsonl"), "utf-8");
  const lines = decisionsRaw.trim().split("\n");
  assert.equal(lines.length, 1);
  const loggedDecision = JSON.parse(lines[0]);
  assert.equal(loggedDecision.status, "promoted");
}

async function testSampleGateBlocksPromotion() {
  const stateDir = await mkdtemp(path.join(tmpdir(), "aether-evolve-gate-"));
  const inputPath = path.join(stateDir, "variant-metrics.json");
  await writeFile(
    inputPath,
    JSON.stringify(
      [
        {
          workflow: "growth",
          variant: "v1",
          success_rate: 0.9,
          sample_size: 3,
          avg_cost_per_run_usd: 1.5,
          p95_latency_ms: 1500,
          safety_score: 0.98
        },
        {
          workflow: "growth",
          variant: "v2",
          success_rate: 0.7,
          sample_size: 20,
          avg_cost_per_run_usd: 6,
          p95_latency_ms: 2800,
          safety_score: 0.99
        }
      ],
      null,
      2
    ),
    "utf-8"
  );

  const decision = await runEvolution({
    workflow: "growth",
    inputPath,
    stateDir,
    minSamples: 10,
    safetyFloor: 0.95,
    now: new Date("2026-03-05T11:00:00.000Z")
  });

  assert.equal(decision.status, "insufficient_samples");
  assert.equal(decision.promoted_variant, null);
  await assert.rejects(access(path.join(stateDir, "active-variant.json")));

  const decisionsRaw = await readFile(path.join(stateDir, "variant-decisions.jsonl"), "utf-8");
  const lines = decisionsRaw.trim().split("\n");
  assert.equal(lines.length, 1);
  const loggedDecision = JSON.parse(lines[0]);
  assert.equal(loggedDecision.status, "insufficient_samples");
}

async function testObservationAggregationAndLatestRunSelection() {
  const stateDir = await mkdtemp(path.join(tmpdir(), "aether-evolve-observations-"));
  const inputPath = path.join(stateDir, "variant-observations.jsonl");
  const lines = [
    JSON.stringify({
      timestamp: "2026-03-05T09:00:00.000Z",
      run_id: "run-1",
      workflow: "growth",
      variant_id: "v1",
      status: "waiting_approval",
      success: false,
      duration_sec: 10,
      total_cost_usd: 0.05
    }),
    JSON.stringify({
      timestamp: "2026-03-05T09:05:00.000Z",
      run_id: "run-1",
      workflow: "growth",
      variant_id: "v1",
      status: "succeeded",
      success: true,
      duration_sec: 34,
      total_cost_usd: 0.08
    }),
    JSON.stringify({
      timestamp: "2026-03-05T09:10:00.000Z",
      run_id: "run-2",
      workflow: "growth",
      variant_id: "v2",
      status: "failed",
      success: false,
      duration_sec: 21,
      total_cost_usd: 0.04
    }),
    JSON.stringify({
      timestamp: "2026-03-05T09:15:00.000Z",
      run_id: "run-3",
      workflow: "growth",
      variant_id: "v1",
      status: "succeeded",
      success: true,
      duration_sec: 37,
      total_cost_usd: 0.09
    })
  ];
  await writeFile(inputPath, `${lines.join("\n")}\n`, "utf-8");

  const decision = await runEvolution({
    workflow: "growth",
    stateDir,
    minSamples: 1,
    safetyFloor: 0.95,
    now: new Date("2026-03-05T12:00:00.000Z")
  });

  assert.equal(decision.input_path.endsWith("variant-observations.jsonl"), true);
  assert.equal(decision.status, "promoted");
  assert.equal(decision.promoted_variant, "v1");
  assert.equal(decision.candidates.length, 2);
  assert.equal(decision.candidates[0].variant_id, "v1");
  assert.equal(decision.candidates[0].sample_size, 2);
  assert.equal(decision.candidates[0].success_rate, 1);
}

main().catch((error) => {
  const message = error instanceof Error ? error.stack ?? error.message : String(error);
  process.stderr.write(`${message}\n`);
  process.exit(1);
});
