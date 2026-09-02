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


def fail_open(client: TrustRouterClient, node: str) -> None:
    for _ in range(OPEN_THRESHOLD):
        client.report_result(node, success=False, latency_ms=30_000)


def main() -> None:
    client = TrustRouterClient(
        base_url=os.environ.get("TRUST_ROUTER_URL", "http://127.0.0.1:7878"),
        tenant="yc-demo",
        api_key=os.environ.get("TRUST_ROUTER_API_KEY", "trust-router-demo-key"),
    )
    wait_for_sidecar(client)

    initial = client.route("start", "done")
    print(f"initial={initial}")
    assert initial["decision"] == "routed"

    fail_open(client, "primary_search")
    fail_open(client, "fallback_search")

    escalated = client.route("start", "done")
    print(f"escalated={escalated}")
    assert escalated["decision"] == "escalate"
    assert "failed_or_blocked_nodes" in escalated

    print("Escalation example verified live sidecar no-path response.")


if __name__ == "__main__":
    main()
