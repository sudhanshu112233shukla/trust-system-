# System Planning

`SystemPlanner` composes three existing deterministic controls without executing
an inference backend:

```text
placement candidates
  -> capacity feasibility
  -> declared backend capability
  -> inference constraints
  -> normalized deterministic score
  -> selected placement or explicit rejections
```

A candidate is selectable only when its model matches the requested capacity,
its compute node is eligible, its named backend exists, and that backend
explicitly declares support for the requested KV strategy. Unknown backends,
unsupported capabilities, unavailable nodes, malformed identifiers, and invalid
scoring configuration are explicit rejections.

The component consumes caller-supplied estimates. It does not claim live GPU
scheduling, execute a backend, or manufacture performance measurements.
Invalid constraints or objective weights fail closed: no placement is selected.
Stable sorting makes both selections and rejection output reproducible.