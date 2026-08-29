from __future__ import annotations

import concurrent.futures
import json
import time
import urllib.parse
import urllib.request


BASE_URL = "http://127.0.0.1:7878"
API_KEY = "trust-router-demo-key"
TASKS = 100
OPS_PER_TASK = 200
TENANT = "yc-demo"
OPEN_THRESHOLD = 5


def get_json(path: str, params: dict[str, object]) -> dict[str, object]:
    query = urllib.parse.urlencode(params)
    request = urllib.request.Request(
        f"{BASE_URL}{path}?{query}",
        headers={"X-API-Key": API_KEY},
    )
    with urllib.request.urlopen(request, timeout=5) as response:
        return json.loads(response.read().decode("utf-8"))


def worker(worker_id: int) -> list[int]:
    latencies: list[int] = []
    for iteration in range(OPS_PER_TASK):
        started = time.perf_counter_ns()
        if (worker_id + iteration) % 3 == 0:
            get_json("/route", {"tenant": TENANT, "start": "start", "goal": "done"})
        else:
            get_json(
                "/result",
                {
                    "tenant": TENANT,
                    "node": "primary_search" if iteration % 2 == 0 else "fallback_search",
                    "success": "true",
                    "latency_ms": 100 + (iteration % 50),
                },
            )
        latencies.append((time.perf_counter_ns() - started) // 1_000)
    return latencies


def inject_failures() -> None:
    for _ in range(8):
        for node in ("primary_search", "fallback_search"):
            for _ in range(OPEN_THRESHOLD):
                get_json(
                    "/result",
                    {
                        "tenant": TENANT,
                        "node": node,
                        "success": "false",
                        "latency_ms": 30_000,
                    },
                )
        time.sleep(0.01)
        for node in ("primary_search", "fallback_search"):
            get_json("/force-half-open", {"tenant": TENANT, "node": node})
            for _ in range(3):
                get_json(
                    "/result",
                    {
                        "tenant": TENANT,
                        "node": node,
                        "success": "true",
                        "latency_ms": 100,
                    },
                )


def percentile(values: list[int], pct: float) -> int:
    if not values:
        return 0
    index = round((len(values) - 1) * pct / 100)
    return sorted(values)[index]


def main() -> None:
    started = time.perf_counter()
    all_latencies: list[int] = []
    panics = 0

    with concurrent.futures.ThreadPoolExecutor(max_workers=TASKS + 1) as executor:
        futures = [executor.submit(worker, worker_id) for worker_id in range(TASKS)]
        failure_future = executor.submit(inject_failures)

        for future in concurrent.futures.as_completed(futures):
            try:
                all_latencies.extend(future.result())
            except Exception as exc:
                panics += 1
                print(f"worker failed: {exc}", flush=True)

        failure_future.result()

    print("Trust Router container HTTP load test")
    print(f"tasks={TASKS}")
    print(f"ops_per_task={OPS_PER_TASK}")
    print(f"total_ops={len(all_latencies)}")
    print(f"elapsed_ms={int((time.perf_counter() - started) * 1000)}")
    print(f"panics={panics}")
    print(f"p50_us={percentile(all_latencies, 50)}")
    print(f"p95_us={percentile(all_latencies, 95)}")
    print(f"p99_us={percentile(all_latencies, 99)}")


if __name__ == "__main__":
    main()
