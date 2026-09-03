from __future__ import annotations

import inspect
import time
from dataclasses import dataclass
from typing import Any, Callable, Mapping

from trust_router_client import TrustRouterClient


ToolCallable = Callable[..., Any]


@dataclass(frozen=True)
class RoutedToolExecutor:
    """Executes named tools in the order returned by Trust Router."""

    router: TrustRouterClient
    tools: Mapping[str, ToolCallable]

    def run(self, start: str, goal: str, *args: Any, **kwargs: Any) -> list[tuple[str, Any]]:
        decision = self.router.route(start, goal)
        if decision.get("decision") == "escalate":
            raise RuntimeError(f"trust router escalation: {decision}")

        outputs: list[tuple[str, Any]] = []
        for node in decision["path"][1:]:
            if node == goal:
                continue
            tool = self.tools.get(node)
            if tool is None:
                continue
            started = time.perf_counter()
            try:
                result = tool(*args, **kwargs)
                self.router.report_result(node, True, _elapsed_ms(started))
                outputs.append((node, result))
            except Exception:
                self.router.report_result(node, False, _elapsed_ms(started))
                raise
        return outputs


@dataclass(frozen=True)
class OpenAIEscalationAdapter:
    """Calls the official OpenAI SDK only after Trust Router returns escalate."""

    client: Any
    model: str

    @classmethod
    def from_env(cls, model: str = "gpt-5-mini") -> "OpenAIEscalationAdapter":
        from openai import OpenAI

        return cls(client=OpenAI(), model=model)

    def recover(self, escalation: dict[str, Any], user_task: str) -> str:
        prompt = (
            "Trust Router could not find a deterministic tool path. "
            "Return a bounded recovery plan.\n"
            f"Task: {user_task}\n"
            f"Escalation: {escalation}"
        )
        response = self.client.responses.create(
            model=self.model,
            input=prompt,
            max_output_tokens=300,
        )
        return response.output_text


@dataclass(frozen=True)
class McpToolInterceptor:
    """Routes MCP call_tool invocations through Trust Router before execution."""

    router: TrustRouterClient
    session: Any

    async def call_tool(self, start: str, goal: str, tool_arguments: Mapping[str, Any]) -> list[Any]:
        from mcp import ClientSession  # noqa: F401 - verifies the real MCP SDK is installed.

        decision = self.router.route(start, goal)
        if decision.get("decision") == "escalate":
            raise RuntimeError(f"trust router escalation: {decision}")

        results: list[Any] = []
        for node in decision["path"][1:]:
            if node == goal:
                continue
            started = time.perf_counter()
            try:
                maybe_result = self.session.call_tool(node, dict(tool_arguments))
                result = await maybe_result if inspect.isawaitable(maybe_result) else maybe_result
                self.router.report_result(node, True, _elapsed_ms(started))
                results.append(result)
            except Exception:
                self.router.report_result(node, False, _elapsed_ms(started))
                raise
        return results


def _elapsed_ms(started: float) -> int:
    return max(0, int((time.perf_counter() - started) * 1000))
