# Decision Traces

A `DecisionTrace` is a bounded, schema-versioned explanation of a
`SystemPlanner` decision. It contains the selected backend/node/model/strategy,
normalized score, bounded fallback order, rejected placements, and unavailable
nodes.

It is intentionally distinct from the append-only JSONL audit record. A trace
is suitable for an API response or telemetry event. It does not include API
keys, headers, request bodies, prompts, tool payloads, or credentials.

Trace IDs use the same 128-character ASCII identifier rules as the sidecar.
At most 64 placement rejections are emitted; `rejected_truncated` tells an
operator when additional rejections were omitted. Reason values are stable,
machine-readable codes rather than backend error strings.