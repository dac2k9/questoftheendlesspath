---
name: POI trigger radius should be configurable
description: Per-POI trigger radius — some POIs (dungeons, camps) should trigger from nearby, villages/shrines require exact tile
type: project
---

POI trigger radius should be configurable per POI, not hardcoded to exact match.

**Why:** Large POIs like dungeons or camps should trigger when nearby, but villages/shrines should require walking to the exact tile. Currently `poi_at()` uses exact match for all.

**How to apply:** Add an optional `trigger_radius` field to `PointOfInterest` (default 0 = exact). Update `poi_at()` to accept a radius parameter or check per-POI. This is a future enhancement — not urgent.
