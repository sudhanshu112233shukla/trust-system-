# Backend Registry

The in-process `BackendRegistry` binds a stable backend ID and version to a declared, machine-checkable model/KV-strategy capability set and a generic `InferenceBackend`. It rejects duplicate IDs and unsupported capabilities before backend execution. This is a deterministic in-process registry, not a distributed service-discovery or scheduling system.