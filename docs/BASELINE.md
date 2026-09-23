# Baseline

The existing `trust-router-baseline` binary compares a deterministic Trust Router control loop with a pure ReAct-style loop. It reports task success and LLM control-plane call counts. The current benchmark runner is:

```powershell
cargo run --bin trust-router-baseline
```

The planner benchmark is separate and has `backend_inference_calls=0`; it measures only deterministic planner and router overhead. Do not compare its microsecond timings with model-inference latency.