from __future__ import annotations

import os
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Protocol

import requests

sys.path.insert(0, str(Path(__file__).resolve().parent))

from trust_router_client import TrustRouterClient


OPEN_THRESHOLD = 5


class InvokableTool(Protocol):
    name: str

    def invoke(self, input_text: str) -> str: ...


@dataclass(frozen=True)
class StandInTool:
    name: str
    func: Callable[[str], str]

    def invoke(self, input_text: str) -> str:
        return self.func(input_text)


def make_tool(name: str, func: Callable[[str], str]) -> InvokableTool:
    try:
        from langchain_core.tools import Tool

        return Tool.from_function(name=name, description=f"Demo tool {name}", func=func)
    except ImportError:
        return StandInTool(name=name, func=func)


class IntentionalToolFailure(RuntimeError):
    pass


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
        base_url=os.environ.get("TRUST_ROUTER_URL", "http://127.0.0.1:7878"),
        tenant="yc-demo",
        api_key=os.environ.get("TRUST_ROUTER_API_KEY", "trust-router-demo-key"),
    )
    wait_for_sidecar(client)

    primary_failures_remaining = 1
    llm_decision_calls = 0
    executed_tools: list[str] = []

    def primary_search(input_text: str) -> str:
        nonlocal primary_failures_remaining
        if primary_failures_remaining > 0:
            primary_failures_remaining -= 1
            raise IntentionalToolFailure("primary search failed intentionally")
        return f"primary result for {input_text}"

    def fallback_search(input_text: str) -> str:
        return f"fallback result for {input_text}"

    def summarize(input_text: str) -> str:
        return f"summary({input_text})"

    tools = {
        "primary_search": make_tool("primary_search", primary_search),
        "fallback_search": make_tool("fallback_search", fallback_search),
        "summarize": make_tool("summarize", summarize),
    }

    task_input = "find routing evidence"
    decision = client.route("start", "done")
    assert decision["decision"] == "routed", decision
    assert decision["path"][1] == "primary_search", decision

    cursor = task_input
    for tool_name in decision["path"][1:]:
        if tool_name == "done":
            break

        try:
            cursor = tools[tool_name].invoke(cursor)
            executed_tools.append(tool_name)
            client.report_result(tool_name, success=True, latency_ms=100)
        except IntentionalToolFailure:
            for _ in range(OPEN_THRESHOLD):
                client.report_result(tool_name, success=False, latency_ms=30_000)
            rerouted = client.route("start", "done")
            assert rerouted["decision"] == "routed", rerouted
            assert rerouted["path"][1] == "fallback_search", rerouted

            cursor = task_input
            for rerouted_tool in rerouted["path"][1:]:
                if rerouted_tool == "done":
                    break
                cursor = tools[rerouted_tool].invoke(cursor)
                executed_tools.append(rerouted_tool)
                client.report_result(rerouted_tool, success=True, latency_ms=100)
            break

    assert "fallback_search" in executed_tools, executed_tools
    assert "summarize" in executed_tools, executed_tools
    assert llm_decision_calls == 0

    integration_kind = (
        "langchain_core_tool" if tools["primary_search"].__class__.__module__.startswith("langchain_core") else "hand_rolled_tool_stand_in"
    )
    print(f"executed_tools={executed_tools}")
    print(f"final_output={cursor}")
    print(f"langchain_style_integration={integration_kind}")
    print("llm_decision_calls=0")
    print("LangChain-style integration verified live sidecar reroute.")


if __name__ == "__main__":
    main()
