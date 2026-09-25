# World State

The `WorldState` abstraction represents the latest known inference environment. 

Every state update produces a monotonically increasing revision. Snapshots are immutable, and the planner can fail closed with `STALE_WORLD_STATE` if the latest observation is too old.
