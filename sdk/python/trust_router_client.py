from __future__ import annotations

from dataclasses import dataclass
from typing import Any

import requests


@dataclass(frozen=True)
class TrustRouterClient:
    """Small sidecar client for wrapping tool-calling loops."""

    base_url: str = "http://127.0.0.1:7878"
    tenant: str = "yc-demo"
    timeout_seconds: float = 5.0
    api_key: str = "trust-router-demo-key"

    def route(self, start: str, goal: str) -> dict[str, Any]:
        response = requests.get(
            f"{self.base_url.rstrip('/')}/route",
            params={"tenant": self.tenant, "start": start, "goal": goal},
            timeout=self.timeout_seconds,
            headers={"X-API-Key": self.api_key},
        )
        response.raise_for_status()
        return response.json()

    def report_result(self, node: str, success: bool, latency_ms: int) -> dict[str, Any]:
        response = requests.post(
            f"{self.base_url.rstrip('/')}/result",
            params={
                "tenant": self.tenant,
                "node": node,
                "success": str(success).lower(),
                "latency_ms": latency_ms,
            },
            timeout=self.timeout_seconds,
            headers={"X-API-Key": self.api_key},
        )
        response.raise_for_status()
        return response.json()


def langchain_style_example() -> None:
    router = TrustRouterClient()

    # In a LangChain-style loop, ask Trust Router for the deterministic path
    # before letting the agent spend tokens deciding routine tool order.
    decision = router.route(start="start", goal="done")
    if decision["decision"] == "escalate":
        # Fall back to the LLM only when the graph has no feasible route.
        return

    for tool_name in decision["path"][1:]:
        try:
            # Replace this comment with: tool_registry[tool_name].invoke(...)
            latency_ms = 0
            router.report_result(tool_name, success=True, latency_ms=latency_ms)
        except Exception:
            router.report_result(tool_name, success=False, latency_ms=30_000)
            raise
