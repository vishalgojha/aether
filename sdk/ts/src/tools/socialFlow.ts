export type SocialFlowScope =
  | "meta:read_accounts"
  | "meta:campaign:read"
  | "meta:campaign:write"
  | "meta:bulk:write";

export interface ApprovalContext {
  runId: string;
  stepId: string;
  actor?: string;
  approved: boolean;
}

export interface RetryPolicy {
  maxAttempts: number;
  baseDelayMs: number;
  maxDelayMs: number;
  retryMultiplier: number;
  jitterRatio: number;
  retryStatuses: number[];
}

export interface CircuitBreakerPolicy {
  failureThreshold: number;
  cooldownMs: number;
}

export interface SocialFlowTelemetryEvent {
  timestamp: string;
  type:
    | "attempt_start"
    | "attempt_success"
    | "attempt_failure"
    | "circuit_open"
    | "circuit_opened";
  operation: string;
  highStakes: boolean;
  attempt?: number;
  maxAttempts?: number;
  status?: number;
  code?: SocialFlowErrorCode;
  retryable?: boolean;
  breakerOpenUntilMs?: number;
}

export interface SocialFlowConfig {
  baseUrl: string;
  apiKey: string;
  scopes: SocialFlowScope[];
  adSpendApprovalUsd: number;
  timeoutMs?: number;
  dryRun?: boolean;
  userAgent?: string;
  retry?: Partial<RetryPolicy>;
  circuitBreaker?: Partial<CircuitBreakerPolicy>;
  onTelemetry?: (event: SocialFlowTelemetryEvent) => void;
}

export interface CampaignPlanInput {
  accountId: string;
  objective: "lead_generation" | "traffic" | "awareness";
  budgetUsd: number;
  audienceHint: string;
}

export interface CampaignPlan {
  objective: string;
  recommendedBudgetUsd: number;
  expectedCtr: number;
  confidence: number;
}

export type SocialFlowErrorCode =
  | "APPROVAL_REQUIRED"
  | "SCOPE_MISSING"
  | "RATE_LIMITED"
  | "AUTH"
  | "RETRYABLE_HTTP"
  | "NON_RETRYABLE_HTTP"
  | "NETWORK"
  | "CIRCUIT_OPEN"
  | "ESCALATION_REQUIRED";

export class SocialFlowError extends Error {
  constructor(
    message: string,
    public readonly code: SocialFlowErrorCode,
    public readonly status?: number,
    public readonly details?: Record<string, unknown>
  ) {
    super(message);
    this.name = "SocialFlowError";
  }
}

interface BreakerState {
  consecutiveFailures: number;
  openUntilMs: number;
}

export class SocialFlowTool {
  private readonly retryPolicy: RetryPolicy;
  private readonly circuitBreakerPolicy: CircuitBreakerPolicy;
  private readonly breakerStates = new Map<string, BreakerState>();

  constructor(private readonly config: SocialFlowConfig) {
    this.retryPolicy = {
      maxAttempts: config.retry?.maxAttempts ?? 4,
      baseDelayMs: config.retry?.baseDelayMs ?? 1000,
      maxDelayMs: config.retry?.maxDelayMs ?? 16_000,
      retryMultiplier: config.retry?.retryMultiplier ?? 4,
      jitterRatio: config.retry?.jitterRatio ?? 0.2,
      retryStatuses: config.retry?.retryStatuses ?? [408, 409, 425, 429, 500, 502, 503, 504]
    };
    this.circuitBreakerPolicy = {
      failureThreshold: config.circuitBreaker?.failureThreshold ?? 3,
      cooldownMs: config.circuitBreaker?.cooldownMs ?? 5 * 60_000
    };
  }

  async listAccounts(): Promise<string[]> {
    this.requireScope("meta:read_accounts");
    const data = await this.callSocialFlow("/v1/meta/accounts", "GET", undefined, {
      operation: "list_accounts",
      highStakes: false
    });
    return (data.accounts as string[]) ?? [];
  }

  async generateCampaignPlan(input: CampaignPlanInput): Promise<CampaignPlan> {
    this.requireScope("meta:campaign:read");
    return {
      objective: input.objective,
      recommendedBudgetUsd: input.budgetUsd,
      expectedCtr: 0.017,
      confidence: 0.76
    };
  }

  async launchCampaign(
    input: CampaignPlanInput,
    approval: ApprovalContext
  ): Promise<{ campaignId: string; status: string }> {
    this.requireScope("meta:campaign:write");
    if (input.budgetUsd > this.config.adSpendApprovalUsd && !approval.approved) {
      throw new SocialFlowError(
        `approval_required: budget ${input.budgetUsd} > threshold ${this.config.adSpendApprovalUsd}`,
        "APPROVAL_REQUIRED"
      );
    }
    const payload = {
      ...input,
      runId: approval.runId,
      stepId: approval.stepId,
      approvedBy: approval.actor ?? null
    };
    const data = await this.callSocialFlow("/v1/meta/campaigns", "POST", payload, {
      operation: "launch_campaign",
      highStakes: true,
      idempotencyKey: this.buildIdempotencyKey("launch_campaign", approval),
      runId: approval.runId,
      stepId: approval.stepId
    });
    return {
      campaignId: String(data.campaignId ?? "stub-campaign-id"),
      status: String(data.status ?? "submitted")
    };
  }

  async bulkPauseCampaigns(
    campaignIds: string[],
    approval: ApprovalContext
  ): Promise<{ affected: number }> {
    this.requireScope("meta:bulk:write");
    if (!approval.approved) {
      throw new SocialFlowError(
        "approval_required: bulk mutation needs human approval",
        "APPROVAL_REQUIRED"
      );
    }
    const data = await this.callSocialFlow(
      "/v1/meta/campaigns/bulk/pause",
      "POST",
      {
        campaignIds,
        runId: approval.runId,
        stepId: approval.stepId
      },
      {
        operation: "bulk_pause_campaigns",
        highStakes: true,
        idempotencyKey: this.buildIdempotencyKey("bulk_pause_campaigns", approval),
        runId: approval.runId,
        stepId: approval.stepId
      }
    );
    return { affected: Number(data.affected ?? campaignIds.length) };
  }

  private async callSocialFlow(
    path: string,
    method: "GET" | "POST",
    body?: unknown,
    ctx?: {
      operation?: string;
      highStakes?: boolean;
      idempotencyKey?: string;
      runId?: string;
      stepId?: string;
    }
  ): Promise<Record<string, unknown>> {
    if (this.config.dryRun && method === "POST") {
      return { status: "dry_run", path };
    }

    const operation = ctx?.operation ?? `${method} ${path}`;
    const highStakes = Boolean(ctx?.highStakes);
    return this.executeWithRetry(operation, highStakes, async () => {
      const controller = new AbortController();
      const timeoutMs = this.config.timeoutMs ?? 15_000;
      const timeout = setTimeout(() => controller.abort(), timeoutMs);
      try {
        const headers: Record<string, string> = {
          "content-type": "application/json",
          authorization: `Bearer ${this.config.apiKey}`,
          "user-agent": this.config.userAgent ?? "aether-socialflow/0.1.0"
        };
        if (ctx?.idempotencyKey && method === "POST") {
          headers["idempotency-key"] = ctx.idempotencyKey;
        }
        if (ctx?.runId) {
          headers["x-aether-run-id"] = ctx.runId;
        }
        if (ctx?.stepId) {
          headers["x-aether-step-id"] = ctx.stepId;
        }

        const response = await fetch(`${this.config.baseUrl}${path}`, {
          method,
          headers,
          body: body ? JSON.stringify(body) : undefined,
          signal: controller.signal
        });
        if (!response.ok) {
          const details = await response.text();
          if (response.status === 401 || response.status === 403) {
            throw new SocialFlowError(
              `socialflow_auth_error: ${response.status} ${details}`,
              "AUTH",
              response.status
            );
          }
          if (response.status === 429) {
            throw new SocialFlowError(
              `socialflow_rate_limited: ${response.status} ${details}`,
              "RATE_LIMITED",
              response.status
            );
          }
          if (this.retryPolicy.retryStatuses.includes(response.status)) {
            throw new SocialFlowError(
              `socialflow_retryable_http_error: ${response.status} ${details}`,
              "RETRYABLE_HTTP",
              response.status
            );
          }
          throw new SocialFlowError(
            `socialflow_http_error: ${response.status} ${details}`,
            "NON_RETRYABLE_HTTP",
            response.status
          );
        }

        const contentType = response.headers.get("content-type") ?? "";
        if (!contentType.includes("application/json")) {
          return { status: "ok", raw: await response.text() };
        }
        return (await response.json()) as Record<string, unknown>;
      } catch (error) {
        if (error instanceof SocialFlowError) {
          throw error;
        }
        const message = error instanceof Error ? error.message : String(error);
        throw new SocialFlowError(`socialflow_network_error: ${message}`, "NETWORK");
      } finally {
        clearTimeout(timeout);
      }
    });
  }

  private requireScope(scope: SocialFlowScope): void {
    if (!this.config.scopes.includes(scope)) {
      throw new SocialFlowError(`scope_missing: ${scope}`, "SCOPE_MISSING");
    }
  }

  private buildIdempotencyKey(operation: string, approval: ApprovalContext): string {
    return `${operation}:${approval.runId}:${approval.stepId}`;
  }

  private async executeWithRetry<T>(
    operation: string,
    highStakes: boolean,
    fn: () => Promise<T>
  ): Promise<T> {
    const breakerState = this.getBreakerState(operation);
    if (breakerState.openUntilMs > Date.now()) {
      this.emitTelemetry({
        timestamp: new Date().toISOString(),
        type: "circuit_open",
        operation,
        highStakes,
        breakerOpenUntilMs: breakerState.openUntilMs
      });
      throw new SocialFlowError(
        `socialflow_circuit_open: operation '${operation}' is temporarily blocked`,
        "CIRCUIT_OPEN",
        undefined,
        { operation, breakerOpenUntilMs: breakerState.openUntilMs }
      );
    }

    let delay = this.retryPolicy.baseDelayMs;
    for (let attempt = 1; attempt <= this.retryPolicy.maxAttempts; attempt += 1) {
      this.emitTelemetry({
        timestamp: new Date().toISOString(),
        type: "attempt_start",
        operation,
        highStakes,
        attempt,
        maxAttempts: this.retryPolicy.maxAttempts
      });
      try {
        const value = await fn();
        this.onOperationSuccess(operation);
        this.emitTelemetry({
          timestamp: new Date().toISOString(),
          type: "attempt_success",
          operation,
          highStakes,
          attempt,
          maxAttempts: this.retryPolicy.maxAttempts
        });
        return value;
      } catch (error) {
        const sfError = normalizeSocialFlowError(error);
        const retryable =
          sfError.code === "NETWORK" ||
          sfError.code === "RATE_LIMITED" ||
          sfError.code === "RETRYABLE_HTTP";
        this.emitTelemetry({
          timestamp: new Date().toISOString(),
          type: "attempt_failure",
          operation,
          highStakes,
          attempt,
          maxAttempts: this.retryPolicy.maxAttempts,
          code: sfError.code,
          status: sfError.status,
          retryable
        });

        if (!retryable || attempt === this.retryPolicy.maxAttempts) {
          this.onOperationFailure(operation, highStakes, sfError);
          if (highStakes && retryable) {
            throw new SocialFlowError(
              `socialflow_escalation_required: retries exhausted for '${operation}'`,
              "ESCALATION_REQUIRED",
              sfError.status,
              {
                operation,
                attempts: attempt,
                causeCode: sfError.code
              }
            );
          }
          throw sfError;
        }

        await sleep(withJitter(delay, this.retryPolicy.jitterRatio));
        delay = Math.min(
          this.retryPolicy.maxDelayMs,
          Math.max(delay, delay * this.retryPolicy.retryMultiplier)
        );
      }
    }
    this.onOperationFailure(
      operation,
      highStakes,
      new SocialFlowError("retry_exhausted", "RETRYABLE_HTTP")
    );
    if (highStakes) {
      throw new SocialFlowError(
        `socialflow_escalation_required: retries exhausted for '${operation}'`,
        "ESCALATION_REQUIRED",
        undefined,
        { operation, attempts: this.retryPolicy.maxAttempts, causeCode: "RETRYABLE_HTTP" }
      );
    }
    throw new SocialFlowError("retry_exhausted", "RETRYABLE_HTTP");
  }

  private getBreakerState(operation: string): BreakerState {
    return this.breakerStates.get(operation) ?? { consecutiveFailures: 0, openUntilMs: 0 };
  }

  private onOperationSuccess(operation: string): void {
    this.breakerStates.set(operation, { consecutiveFailures: 0, openUntilMs: 0 });
  }

  private onOperationFailure(
    operation: string,
    highStakes: boolean,
    error: SocialFlowError
  ): void {
    if (!isBreakerFailure(error)) {
      return;
    }
    const current = this.getBreakerState(operation);
    const next: BreakerState = {
      consecutiveFailures: current.consecutiveFailures + 1,
      openUntilMs: current.openUntilMs
    };

    if (next.consecutiveFailures >= this.circuitBreakerPolicy.failureThreshold) {
      next.openUntilMs = Date.now() + this.circuitBreakerPolicy.cooldownMs;
      this.emitTelemetry({
        timestamp: new Date().toISOString(),
        type: "circuit_opened",
        operation,
        highStakes,
        code: error.code,
        status: error.status,
        breakerOpenUntilMs: next.openUntilMs
      });
    }
    this.breakerStates.set(operation, next);
  }

  private emitTelemetry(event: SocialFlowTelemetryEvent): void {
    this.config.onTelemetry?.(event);
  }
}

function normalizeSocialFlowError(error: unknown): SocialFlowError {
  if (error instanceof SocialFlowError) {
    return error;
  }
  const message = error instanceof Error ? error.message : String(error);
  return new SocialFlowError(message, "NETWORK");
}

function isBreakerFailure(error: SocialFlowError): boolean {
  return (
    error.code === "NETWORK" ||
    error.code === "RATE_LIMITED" ||
    error.code === "RETRYABLE_HTTP"
  );
}

function withJitter(baseMs: number, ratio: number): number {
  const amplitude = baseMs * ratio;
  const offset = (Math.random() * 2 - 1) * amplitude;
  return Math.max(0, Math.round(baseMs + offset));
}

async function sleep(ms: number): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, ms));
}
