from __future__ import annotations

import os
import sys
import time
from pathlib import Path

import requests

sys.path.insert(0, str(Path(__file__).resolve().parent))

from trust_router_client import TrustRouterClient


OPEN_THRESHOLD = 5


def wait_for_sidecar(client: TrustRouterClient) -> None:
    deadline = time.time() + 10
    while time.time() < deadline:
        try:
            response = requests.get(
                f"{client.base_url.rstrip('/')}/healthz",
                timeout=client.timeout_seconds,
            )
            if response.ok and response.json().get("ok") is True:
                return
        except requests.RequestException:
            time.sleep(0.2)

    raise RuntimeError(f"sidecar did not become ready at {client.base_url}")


def main() -> None:
    client = TrustRouterClient(
        base_url="http://127.0.0.1:7878",
        tenant="yc-demo",
        api_key=os.environ.get("TRUST_ROUTER_API_KEY", "trust-router-demo-key"),
    )
    wait_for_sidecar(client)

    initial_plan = client.plan("start", "done")
    print(f"initial_plan={initial_plan}")
    assert initial_plan["decision"] == "execute"
    assert initial_plan["steps"] == ["start", "primary_search", "summarize", "done"]

    initial = client.route("start", "done")
    print(f"initial={initial}")
    assert initial["decision"] == "routed"
    assert initial["path"] == ["start", "primary_search", "summarize", "done"]

    for _ in range(OPEN_THRESHOLD):
        client.report_result("primary_search", success=False, latency_ms=30_000)

    rerouted = client.route("start", "done")
    print(f"rerouted={rerouted}")
    assert rerouted["decision"] == "routed"
    assert rerouted["path"] == ["start", "fallback_search", "summarize", "done"]

    print("SDK example verified live sidecar reroute.")


if __name__ == "__main__":
    main()
