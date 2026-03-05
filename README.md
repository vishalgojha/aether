# Aether

Aether is a local-first, production-first multi-agent orchestration engine for real business workflows.

This repository starts with:

- Rust runtime core for orchestration, governance gates, audit ledger, replay, and observability.
- TypeScript CLI and SDK for operator workflows and tool integrations.
- Monitoring and deployment scaffolding (Prometheus, Grafana, Qdrant, Docker Compose).

## Quick Start

Prerequisites:

- Rust toolchain (`rustup` + Visual C++ Build Tools on Windows for `link.exe`)
- Node.js 22+

1. Run the core:

```powershell
cd crates/aether-core
cargo run
```

2. Run the CLI:

```powershell
cd cli
npm install
npm run build
node dist/index.js run --workflow growth
node dist/index.js runs
node dist/index.js pending-approvals
node dist/index.js approve --run-id <RUN_ID> --step-id <STEP_ID> --reason "Reviewed risk controls; approved"
```

3. See metrics:

```powershell
curl http://127.0.0.1:8080/metrics
```

4. Open built-in ops UI:

```powershell
start http://127.0.0.1:8080/ui
```

## Production Principles

- Structured JSON logs and traces for every run/step.
- Hard token and cost budgets with kill-switch and approval gates.
- Immutable hash-chained audit events in SQLite.
- Retry and timeout protections to avoid infinite loops and runaway failures.
- Replay endpoint to continue from persisted state after crashes.

## Lightweight Evolution Loop

Use metrics to select safer, cheaper variants:

```powershell
node scripts/evolution-loop.mjs state/variant-metrics.json
```
