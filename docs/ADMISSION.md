# Local Admission Control

`AdmissionController` is a small, thread-safe guard placed in front of local
planning or execution work. It bounds both process-wide and per-tenant
in-flight requests. A successful admission returns an RAII permit; dropping
the permit releases the slot, including during error unwinding.

This is intentionally a single-process control. It prevents one tenant from
exhausting one sidecar instance, but it does not claim cluster-wide fairness,
distributed quotas, or tenant billing. Those require an authoritative shared
quota service and are not implemented.

The default policy permits at most 1,024 local in-flight requests and 64 for a
