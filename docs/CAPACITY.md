# Capacity Snapshots

`CapacitySnapshot` is an in-process simulated resource view. Nodes carry declared region, accelerator label, memory, utilization, queue depth, health, and model residency. A requirement deterministically filters nodes before optimization. It does not collect real GPU measurements, allocate work, or provide distributed scheduling.