export type RouteDecision =
  | {
      decision: "routed";
      path: string[];
      total_cost: number;
      cache_hit: boolean;
    }
  | {
      decision: "escalate";
      reason: string;
      failed_or_blocked_nodes: string[];
    };

export type PlanDecision =
  | {
      decision: "execute";
      schema_version: number;
      path: string[];
      total_cost: number;
      cache_hit: boolean;
      steps: string[];
    }
  | {
      decision: "recover";
      reason: string;
      failed_or_blocked_nodes: string[];
      recovery_steps: string[];
    };

export type ResultResponse = {
  ok: boolean;
};

export type TrustRouterClientOptions = {
  baseUrl?: string;
  tenant?: string;
  timeoutMs?: number;
  apiKey?: string;
};

export class TrustRouterClient {
  private readonly baseUrl: string;
  private readonly tenant: string;
  private readonly timeoutMs: number;
  private readonly apiKey: string;

  constructor(options: TrustRouterClientOptions = {}) {
    this.baseUrl = (options.baseUrl ?? "http://127.0.0.1:7878").replace(/\/+$/, "");
    this.tenant = options.tenant ?? "yc-demo";
    this.timeoutMs = options.timeoutMs ?? 5_000;
    this.apiKey = options.apiKey ?? "trust-router-demo-key";
  }

  async route(start: string, goal: string): Promise<RouteDecision> {
    const params = new URLSearchParams({
      tenant: this.tenant,
      start,
      goal,
    });

    return this.request<RouteDecision>(`/route?${params.toString()}`, {
      method: "GET",
    });
  }

  async plan(start: string, goal: string): Promise<PlanDecision> {
    const params = new URLSearchParams({
      tenant: this.tenant,
      start,
      goal,
    });

    return this.request<PlanDecision>(`/plan?${params.toString()}`, {
      method: "GET",
    });
  }

  async reportResult(
    node: string,
    success: boolean,
    latencyMs: number,
  ): Promise<ResultResponse> {
    const params = new URLSearchParams({
      tenant: this.tenant,
      node,
      success: String(success),
      latency_ms: String(latencyMs),
    });

    return this.request<ResultResponse>(`/result?${params.toString()}`, {
      method: "POST",
    });
  }

  private async request<T>(path: string, init: RequestInit): Promise<T> {
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), this.timeoutMs);

    try {
      const response = await fetch(`${this.baseUrl}${path}`, {
        ...init,
        headers: {
          ...(init.headers ?? {}),
          "X-API-Key": this.apiKey,
        },
        signal: controller.signal,
      });
      const body = await response.text();

      if (!response.ok) {
        throw new Error(`Trust Router request failed: ${response.status} ${body}`);
      }

      return JSON.parse(body) as T;
    } finally {
      clearTimeout(timeout);
    }
  }
}
