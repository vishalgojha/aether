#!/usr/bin/env node
import assert from "node:assert/strict";

import { SocialFlowError, SocialFlowTool } from "../dist/tools/socialFlow.js";

const originalFetch = globalThis.fetch;

async function main() {
  try {
    await testRetriesOnRateLimitThenSucceeds();
    await testCircuitBreakerOpensAfterConsecutiveFailures();
    await testHighStakesRetryExhaustionEscalates();
    process.stdout.write("socialflow resilience tests passed\n");
  } finally {
    globalThis.fetch = originalFetch;
  }
}

async function testRetriesOnRateLimitThenSucceeds() {
  const telemetry = [];
  let calls = 0;
  globalThis.fetch = async () => {
    calls += 1;
    if (calls < 3) {
      return new Response("rate limited", {
        status: 429,
        headers: { "content-type": "text/plain" }
      });
    }
    return new Response(JSON.stringify({ accounts: ["acct-1", "acct-2"] }), {
      status: 200,
      headers: { "content-type": "application/json" }
    });
  };

  const tool = new SocialFlowTool({
    baseUrl: "http://localhost:3000",
    apiKey: "test-key",
    scopes: ["meta:read_accounts"],
    adSpendApprovalUsd: 250,
    retry: {
      maxAttempts: 4,
      baseDelayMs: 1,
      maxDelayMs: 2,
      retryMultiplier: 2,
      jitterRatio: 0
    },
    onTelemetry: (event) => telemetry.push(event)
  });

  const accounts = await tool.listAccounts();
  assert.deepEqual(accounts, ["acct-1", "acct-2"]);
  assert.equal(calls, 3);
  assert.equal(
    telemetry.some((event) => event.type === "attempt_failure" && event.code === "RATE_LIMITED"),
    true
  );
  assert.equal(
    telemetry.some((event) => event.type === "attempt_success" && event.attempt === 3),
    true
  );
}

async function testCircuitBreakerOpensAfterConsecutiveFailures() {
  let calls = 0;
  globalThis.fetch = async () => {
    calls += 1;
    return new Response("upstream down", {
      status: 503,
      headers: { "content-type": "text/plain" }
    });
  };

  const tool = new SocialFlowTool({
    baseUrl: "http://localhost:3000",
    apiKey: "test-key",
    scopes: ["meta:read_accounts"],
    adSpendApprovalUsd: 250,
    retry: {
      maxAttempts: 1,
      baseDelayMs: 1,
      maxDelayMs: 1,
      retryMultiplier: 1,
      jitterRatio: 0
    },
    circuitBreaker: {
      failureThreshold: 3,
      cooldownMs: 60_000
    }
  });

  for (let i = 0; i < 3; i += 1) {
    await assert.rejects(
      () => tool.listAccounts(),
      (error) => error instanceof SocialFlowError && error.code === "RETRYABLE_HTTP"
    );
  }
  assert.equal(calls, 3);

  await assert.rejects(
    () => tool.listAccounts(),
    (error) => error instanceof SocialFlowError && error.code === "CIRCUIT_OPEN"
  );
  assert.equal(calls, 3);
}

async function testHighStakesRetryExhaustionEscalates() {
  let calls = 0;
  globalThis.fetch = async () => {
    calls += 1;
    return new Response("retryable", {
      status: 503,
      headers: { "content-type": "text/plain" }
    });
  };

  const tool = new SocialFlowTool({
    baseUrl: "http://localhost:3000",
    apiKey: "test-key",
    scopes: ["meta:campaign:write"],
    adSpendApprovalUsd: 1_000,
    retry: {
      maxAttempts: 2,
      baseDelayMs: 1,
      maxDelayMs: 1,
      retryMultiplier: 1,
      jitterRatio: 0
    },
    circuitBreaker: {
      failureThreshold: 100,
      cooldownMs: 60_000
    }
  });

  await assert.rejects(
    () =>
      tool.launchCampaign(
        {
          accountId: "acct-1",
          objective: "lead_generation",
          budgetUsd: 200,
          audienceHint: "home buyers"
        },
        {
          runId: "run-1",
          stepId: "step-1",
          actor: "ops",
          approved: true
        }
      ),
    (error) => {
      if (!(error instanceof SocialFlowError)) {
        return false;
      }
      assert.equal(error.code, "ESCALATION_REQUIRED");
      assert.equal(error.details?.operation, "launch_campaign");
      assert.equal(error.details?.attempts, 2);
      return true;
    }
  );
  assert.equal(calls, 2);
}

main().catch((error) => {
  const message = error instanceof Error ? error.stack ?? error.message : String(error);
  process.stderr.write(`${message}\n`);
  process.exit(1);
});
