from __future__ import annotations

import json
import time
import urllib.parse
import urllib.request


BASE_URL = "http://trust-router-sidecar:7878"
API_KEY = "trust-router-demo-key"
TENANT = "yc-demo"
OPEN_THRESHOLD = 5


def get_json(path: str, params: dict[str, object] | None = None) -> dict[str, object]:
    query = ""
    if params:
        query = "?" + urllib.parse.urlencode(params)

    request = urllib.request.Request(
        f"{BASE_URL}{path}{query}",
        headers={"X-API-Key": API_KEY},
    )
    with urllib.request.urlopen(request, timeout=5) as response:
        return json.loads(response.read().decode("utf-8"))


def wait_for_sidecar() -> None:
    deadline = time.time() + 30
    while time.time() < deadline:
        try:
            health = get_json("/healthz")
            if health.get("ok") is True:
                return
        except Exception:
            time.sleep(0.5)

    raise RuntimeError("sidecar did not become healthy")


def route() -> dict[str, object]:
    return get_json(
        "/route",
        {"tenant": TENANT, "start": "start", "goal": "done"},
    )


def report_result(node: str, success: bool, latency_ms: int) -> dict[str, object]:
    return get_json(
        "/result",
        {
            "tenant": TENANT,
            "node": node,
            "success": str(success).lower(),
            "latency_ms": latency_ms,
        },
    )


def main() -> None:
    wait_for_sidecar()

    initial = route()
    print(f"initial={initial}", flush=True)

    for _ in range(OPEN_THRESHOLD):
        report_result("primary_search", False, 30_000)

    rerouted = route()
    print(f"rerouted={rerouted}", flush=True)

    if initial.get("decision") != "routed":
        raise RuntimeError(f"expected initial route, got {initial}")
    if rerouted.get("decision") != "routed":
        raise RuntimeError(f"expected reroute, got {rerouted}")
    if rerouted.get("path") != ["start", "fallback_search", "summarize", "done"]:
        raise RuntimeError(f"expected fallback path, got {rerouted}")

    print("compose agent verified sidecar reroute", flush=True)


if __name__ == "__main__":
    main()
