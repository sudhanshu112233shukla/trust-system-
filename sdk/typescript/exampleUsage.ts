import { TrustRouterClient, type RouteDecision } from "./trustRouterClient.ts";

const OPEN_THRESHOLD = 5;

async function waitForSidecar(baseUrl: string): Promise<void> {
  const deadline = Date.now() + 10_000;
  const normalizedBaseUrl = baseUrl.replace(/\/+$/, "");

  while (Date.now() < deadline) {
    try {
      const response = await fetch(`${normalizedBaseUrl}/healthz`);
      const body = (await response.json()) as { ok?: boolean };
      if (response.ok && body.ok === true) {
        return;
      }
    } catch {
      await new Promise((resolve) => setTimeout(resolve, 200));
    }
  }

  throw new Error(`sidecar did not become ready at ${baseUrl}`);
}

function assertPath(decision: RouteDecision, expected: string[]): void {
  if (
    decision.decision !== "routed" ||
    JSON.stringify(decision.path) !== JSON.stringify(expected)
  ) {
    throw new Error(
      `expected routed path ${JSON.stringify(expected)}, got ${JSON.stringify(decision)}`,
    );
  }
}

async function main(): Promise<void> {
  const baseUrl = process.env.TRUST_ROUTER_URL ?? "http://127.0.0.1:7878";
  const client = new TrustRouterClient({ baseUrl, tenant: "yc-demo", apiKey: process.env.TRUST_ROUTER_API_KEY ?? "trust-router-demo-key" });

  await waitForSidecar(baseUrl);

  const initial = await client.route("start", "done");
  console.log(`initial=${JSON.stringify(initial)}`);
  assertPath(initial, ["start", "primary_search", "summarize", "done"]);

  for (let i = 0; i < OPEN_THRESHOLD; i += 1) {
    await client.reportResult("primary_search", false, 30_000);
  }

  const rerouted = await client.route("start", "done");
  console.log(`rerouted=${JSON.stringify(rerouted)}`);
  assertPath(rerouted, ["start", "fallback_search", "summarize", "done"]);

  console.log("TypeScript SDK example verified live sidecar reroute.");
}

main().catch((error: unknown) => {
  console.error(error);
  process.exit(1);
});

