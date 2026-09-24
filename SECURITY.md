# Security

## Security Boundary

Trust Router is a single-process control plane. It validates and authorizes requests before it uses tenant state; it does not terminate TLS, provide multi-principal identity, execute arbitrary tools, or coordinate multiple instances.

## Implemented Controls

- `/healthz` is the only public endpoint. Protected endpoints require `X-API-Key`; comparisons avoid early exit.
- Tenant allow-list authorization applies to every tenant-scoped endpoint.
- Request identifiers are restricted to 1-128 ASCII safe characters, preventing control-character log injection.
- Token-bucket rate limiting returns `429` and `Retry-After` before routing work is performed.
- Errors have a stable, non-secret JSON shape. Responses include request IDs and `X-Content-Type-Options: nosniff`.
- Production startup rejects missing/demo/short secrets, unsafe configuration, and missing allow-lists. Use `TRUST_ROUTER_API_KEY` or `TRUST_ROUTER_API_KEY_FILE`.
- JSONL audit events use `serde_json`, are synced before route/plan responses, and exclude keys, headers, and payload secrets.
- CI runs formatting, lint, tests, documentation build, SDK syntax/type checks, dependency review on pull requests, secret scanning, and Docker image build.
- Compose defaults drop Linux capabilities, prevent privilege escalation, use a read-only root filesystem, and retain only the mounted audit volume as writable state.

## Deployment Requirements

Terminate TLS at a reverse proxy, ingress, or service mesh. Inject credentials through an environment secret or mounted secret file. Do not commit `secrets/`, `.env`, PEM files, or keys. Restrict network access so only authorized clients can reach the sidecar.

## Residual Risks

Shared-key authentication is not per-user identity; state is in-memory; audit JSONL is not tamper-evident; and multiple sidecars do not coordinate health or cache state. The Docker image is built in CI, but local runtime verification remains **NOT VERIFIED** until a Docker daemon is available.

## Vulnerability Reporting

Report vulnerabilities privately to the project owner with affected version, reproduction steps, expected and observed behavior, and sanitized logs. Do not open public issues containing credentials or exploit details.