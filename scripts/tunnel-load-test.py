#!/usr/bin/env python3
"""Load-test the public Coding Tools MCP tunnel with concurrent read-only probes.

The access token is read from an environment variable and is never printed.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import json
import os
import statistics
import sys
import threading
import time
import urllib.error
import urllib.request
from collections import Counter
from dataclasses import dataclass
from typing import Any

PROTOCOL_VERSION = "2025-11-25"
DEFAULT_TOKEN_ENV = "CODING_TOOLS_MCP_ACCESS_TOKEN"


@dataclass(frozen=True)
class ProbeResult:
    outcome: str
    status: int | None
    latency_ms: int
    queue_wait_ms: int | None
    tunnel_error: str | None


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Send synchronized public MCP requests and summarize tunnel capacity behavior."
    )
    parser.add_argument("--endpoint", required=True, help="Public MCP endpoint ending in /mcp")
    parser.add_argument("--workspace-folder-id", required=True)
    parser.add_argument("--token-env", default=DEFAULT_TOKEN_ENV)
    parser.add_argument("--concurrency", type=int, default=20)
    parser.add_argument("--duration-seconds", type=int, default=45)
    parser.add_argument("--request-timeout-seconds", type=int, default=150)
    parser.add_argument("--program", default="pwsh")
    parser.add_argument(
        "--command-template",
        default="Start-Sleep -Seconds {duration}; Write-Output 'tunnel-load-test-ok'",
        help="Command string; {duration} is replaced with --duration-seconds",
    )
    parser.add_argument(
        "--fail-on-capacity",
        action="store_true",
        help="Return non-zero when expected 503 capacity protection occurs",
    )
    args = parser.parse_args()
    if not args.endpoint.startswith(("https://", "http://")):
        parser.error("--endpoint must be an HTTP(S) URL")
    if not args.endpoint.rstrip("/").endswith("/mcp"):
        parser.error("--endpoint must end in /mcp")
    if not 1 <= args.concurrency <= 256:
        parser.error("--concurrency must be between 1 and 256")
    if not 1 <= args.duration_seconds <= 600:
        parser.error("--duration-seconds must be between 1 and 600")
    if args.request_timeout_seconds <= args.duration_seconds:
        parser.error("--request-timeout-seconds must exceed --duration-seconds")
    return args


def mcp_call(
    endpoint: str,
    token: str,
    request_id: int,
    tool: str,
    arguments: dict[str, Any],
    timeout_seconds: int,
) -> tuple[int, dict[str, str], dict[str, Any]]:
    payload = json.dumps(
        {
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "tools/call",
            "params": {"name": tool, "arguments": arguments},
        }
    ).encode("utf-8")
    request = urllib.request.Request(
        endpoint,
        data=payload,
        method="POST",
        headers={
            "Authorization": f"Bearer {token}",
            "Content-Type": "application/json",
            "Accept": "application/json, text/event-stream",
            "MCP-Protocol-Version": PROTOCOL_VERSION,
            "User-Agent": "coding-tools-tunnel-load-test",
            "Connection": "close",
        },
    )
    with urllib.request.urlopen(request, timeout=timeout_seconds) as response:
        raw = response.read()
        headers = {key.lower(): value for key, value in response.headers.items()}
        return response.status, headers, json.loads(raw.decode("utf-8"))


def percentile(values: list[int], percent: int) -> int | None:
    if not values:
        return None
    ordered = sorted(values)
    index = max(0, min(len(ordered) - 1, (len(ordered) * percent + 99) // 100 - 1))
    return ordered[index]


def classify_rpc(payload: dict[str, Any]) -> str:
    if "error" in payload:
        return "rpc_error"
    result = payload.get("result")
    if not isinstance(result, dict):
        return "invalid_rpc_response"
    if result.get("isError") is True:
        return "tool_error"
    return "success"


def main() -> int:
    args = parse_args()
    token = os.environ.get(args.token_env, "").strip()
    if not token:
        print(
            json.dumps(
                {"ok": False, "error": f"missing access token environment variable: {args.token_env}"}
            ),
            file=sys.stderr,
        )
        return 2

    endpoint = args.endpoint.rstrip("/")
    try:
        status, _, payload = mcp_call(
            endpoint,
            token,
            1,
            "switch_workspace_folder",
            {"folder_id": args.workspace_folder_id},
            60,
        )
        if status != 200 or classify_rpc(payload) != "success":
            raise RuntimeError("workspace selection failed")
    except Exception as error:  # noqa: BLE001 - produce one bounded diagnostic
        print(json.dumps({"ok": False, "error": f"setup failed: {type(error).__name__}"}), file=sys.stderr)
        return 2

    barrier = threading.Barrier(args.concurrency)
    command = args.command_template.format(duration=args.duration_seconds)

    def run_probe(index: int) -> ProbeResult:
        barrier.wait(timeout=30)
        started = time.perf_counter()
        try:
            status, headers, payload = mcp_call(
                endpoint,
                token,
                10_000 + index,
                "exec_command",
                {
                    "workspace_folder_id": args.workspace_folder_id,
                    "program": args.program,
                    "args": ["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", command],
                    "yield_time_ms": 30_000,
                    "timeout_ms": min(600_000, args.request_timeout_seconds * 1_000),
                    "output_mode": "none",
                    "reason": "Measure public tunnel concurrency and capacity behavior.",
                },
                args.request_timeout_seconds,
            )
            queue_wait = headers.get("x-tunnel-queue-wait-ms")
            return ProbeResult(
                outcome=classify_rpc(payload),
                status=status,
                latency_ms=round((time.perf_counter() - started) * 1000),
                queue_wait_ms=int(queue_wait) if queue_wait and queue_wait.isdigit() else None,
                tunnel_error=headers.get("x-tunnel-error"),
            )
        except urllib.error.HTTPError as error:
            return ProbeResult(
                outcome=f"http_{error.code}",
                status=error.code,
                latency_ms=round((time.perf_counter() - started) * 1000),
                queue_wait_ms=None,
                tunnel_error=error.headers.get("X-Tunnel-Error"),
            )
        except TimeoutError:
            return ProbeResult("client_timeout", None, round((time.perf_counter() - started) * 1000), None, None)
        except urllib.error.URLError:
            return ProbeResult("transport_error", None, round((time.perf_counter() - started) * 1000), None, None)
        except Exception:  # noqa: BLE001 - result is intentionally classified without secrets
            return ProbeResult("unexpected_error", None, round((time.perf_counter() - started) * 1000), None, None)

    wall_started = time.perf_counter()
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.concurrency) as executor:
        results = list(executor.map(run_probe, range(args.concurrency)))
    wall_ms = round((time.perf_counter() - wall_started) * 1000)

    outcomes = Counter(result.outcome for result in results)
    tunnel_errors = Counter(result.tunnel_error for result in results if result.tunnel_error)
    latencies = [result.latency_ms for result in results]
    queue_waits = [result.queue_wait_ms for result in results if result.queue_wait_ms is not None]
    unexpected = sum(
        count
        for outcome, count in outcomes.items()
        if outcome not in {"success", "http_503"}
    )
    capacity_errors = outcomes.get("http_503", 0)
    report = {
        "ok": unexpected == 0 and (capacity_errors == 0 or not args.fail_on_capacity),
        "endpoint_redacted": endpoint.rsplit("/", 1)[-1],
        "concurrency": args.concurrency,
        "duration_seconds": args.duration_seconds,
        "wall_ms": wall_ms,
        "requests_started": len(results),
        "requests_completed": sum(outcomes.values()),
        "outcomes": dict(sorted(outcomes.items())),
        "tunnel_errors": dict(sorted(tunnel_errors.items())),
        "latency_ms": {
            "min": min(latencies),
            "mean": round(statistics.fmean(latencies), 1),
            "p50": percentile(latencies, 50),
            "p95": percentile(latencies, 95),
            "max": max(latencies),
        },
        "queue_wait_ms": {
            "samples": len(queue_waits),
            "p50": percentile(queue_waits, 50),
            "p95": percentile(queue_waits, 95),
            "max": max(queue_waits) if queue_waits else None,
        },
        "unexpected_failure_count": unexpected,
        "capacity_protection_count": capacity_errors,
    }
    print(json.dumps(report, ensure_ascii=False, indent=2, sort_keys=True))
    return 0 if report["ok"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
