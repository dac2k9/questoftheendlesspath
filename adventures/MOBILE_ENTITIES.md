# Mobile Entities — Design Spec

Status: **planning**, not implemented. Lock-in doc for the next adventure.

## Goal

Server-authoritative autonomous entities (monsters, NPCs, animals) that move
around the overworld and interiors, hooking into the existing event / combat /
dialogue systems without a major rewrite.

## MVP scope

- **Wander** and **Patrol** behaviors only.
- One mobile monster + one mobile NPC test entity in the existing seed12345 world.
- Combat trigger when player steps onto entity tile (same as today's static monsters).
- Dialogue trigger when player ≤ 1 tile away from NPC entity.
- Server tick advances entities. Client polls `/entities`, interpolates between
  polls (mirrors `OtherPlayerSprite`).

Phase 2: Follow-path, Chase, Flee. Phase 3: schedules + day/night.

## Authoring (JSON)

Entities live in `adventures/seed{N}_entities.json` next to events. Loaded at
server startup like events. `ENTITIES_PATH` env var to override (mirrors
`EVENTS_PATH`).

### Core shape

```json
{
  "entities": [
    {
      "id": "forest_wolf_1",
      "kind": "monster",
      "sprite": "wolf",
      "spawn": [30, 40],
      "behavior": { "type": "wander", "radius": 4 },
      "movement": { "speed_tiles_per_min": 6 },
      "on_contact": { "type": "combat", "difficulty": 2 },
      "respawn_after_secs": 600
    }
  ]
}
```

### Field reference

| Field | Type | Notes |
|---|---|---|
| `id` | string | Stable across restarts; used as save key. Must be unique per world. |
| `kind` | enum | `monster` \| `npc` \| `animal` \| (Phase 3) `hazard` |
| `sprite` | string | Asset reference. Maps to a sprite-sheet path the client knows about. |
| `spawn` | `[x, y]` | Initial / respawn tile. |
| `behavior` | tagged enum | See below. |
| `movement.speed_tiles_per_min` | int | Default 6 (1 tile / 10 s). 30 ≈ player at 3 km/h. |
| `on_contact` | tagged enum | What happens when the player and the entity occupy the same tile (or are adjacent for NPCs). See below. |
| `respawn_after_secs` | int? | Null = permanent kill. Common: 600 for monsters, null for unique NPCs. |

### Behavior variants (MVP)

```json
// Wander: random walkable step each tick, biased to stay within `radius` of spawn.
{ "type": "wander", "radius": 4 }

// Patrol: loop through waypoints; bounce or wrap.
{ "type": "patrol", "waypoints": [[30, 40], [33, 40], [33, 43], [30, 43]], "loop_mode": "wrap" }
```

### Behavior variants (Phase 2)

```json
// Follow-path: walk a route from spawn to `to`. `loop`: restart vs despawn.
{ "type": "follow_path", "to": [80, 50], "loop": true }

// Reactive layer (chase / flee): wraps a base behavior.
{
  "behavior": { "type": "wander", "radius": 4 },
  "react_to_player": {
    "type": "chase",
    "trigger_radius": 6,
    "give_up_radius": 12
  }
}
```

While no player is within `trigger_radius`, the base behavior runs. When a
player enters, the reactive override takes priority until they leave
`give_up_radius`.

### `on_contact` variants

```json
{ "type": "combat", "difficulty": 2 }                       // monsters
{ "type": "dialogue", "event_id": "baker_intro" }            // NPCs
{ "type": "trade", "shop_event_id": "village_baker_shop" }   // shopkeeper NPCs
{ "type": "none" }                                           // animals
```

Combat maps to a synthetic `EventKind::RandomEncounter` (or `Boss` if difficulty
≥ 6). Reuses existing combat init / victory / drop tables, just with the
entity's display name and a `mobile_monster:{entity.id}` event id.

## Server-side data model

### Runtime types (questlib::mobile_entity)

```rust
pub struct MobileEntity {
    pub id: String,
    pub kind: EntityKind,
    pub sprite: String,
    pub spawn: (usize, usize),
    pub current: (usize, usize),
    pub facing: Facing,
    pub behavior: Behavior,
    pub on_contact: ContactAction,
    pub speed_tiles_per_min: u32,
    pub respawn_after_secs: Option<u32>,
    pub state: RuntimeState,
}

pub enum RuntimeState {
    Alive { last_step_unix_ms: u64, behavior_state: BehaviorState },
    Dead { respawn_at_unix_ms: u64 },
}

pub enum BehaviorState {
    Wander,                                          // stateless
    Patrol { idx: usize, forward: bool },            // current waypoint
    FollowPath { idx: usize },                       // (Phase 2)
}
```

### Save format

```json
"mobile_entities": {
  "forest_wolf_1": {
    "current": [32, 41],
    "facing": "east",
    "state": { "alive": { "last_step_unix_ms": 1745930000000, "behavior_state": "wander" } }
  }
}
```

Authored fields (id, kind, sprite, spawn, behavior config, contact action) load
fresh from JSON on every startup. Save carries only mutable runtime state.
Old saves load with empty `mobile_entities`; the JSON's authored entities
populate it on startup.

`#[serde(default)]` on the new field gives free forward-compat.

## Tick loop

Sequenced into the existing `gamemaster::tick::run_tick_dev`, **after** player
position advancement, **before** event triggers:

```
for entity in mobile_entities:
    match entity.state:
        Dead { respawn_at }:
            if now ≥ respawn_at: respawn(entity)
            continue
        Alive:
            if now - last_step < (60_000 / speed_tiles_per_min): continue
            next = pick_next_tile(entity, world, players)
            if next != entity.current:
                entity.facing = direction(entity.current → next)
                entity.current = next
                entity.last_step_unix_ms = now
                handle_contact(entity, players_on_tile(next))
```

### `pick_next_tile`

- **Wander:** pick random walkable 4-neighbor; if entity is outside `radius`
  of spawn, restrict candidates to those that move it closer.
- **Patrol:** BFS one step toward `waypoints[idx]` over walkable tiles; on
  reaching the waypoint, advance idx according to `loop_mode`.
- **Follow-path:** pre-computed route, advance idx by 1 per tick step.

### Walkability

Same `route::tile_cost` function players use; impassable tiles are skipped.
Phase 2: per-entity biome restrictions (wolves avoid water; fish only in water).

### Tick rate sufficiency

Existing 3 s server tick × max 200 entities = trivial cost. If we need finer
movement we either lower the tick interval or run a sub-loop just for
entities. Not needed for MVP.

## Network

### `GET /entities?player_id=X`

Returns entities within ~20 tiles of player X (Chebyshev distance). Dead
entities filtered out. Polled every 1 s by the client (same cadence as
`/players`).

```json
{
  "entities": [
    { "id": "forest_wolf_1", "sprite": "wolf", "x": 32, "y": 41, "facing": "east" }
  ]
}
```

Visible-radius filter keeps payload tiny even with hundreds of entities in the
world.

### Why include `sprite` in payload (not just id)?

So the client doesn't need to load the full entity catalog upfront — it
discovers what to render lazily as entities come into view. New adventures can
ship with new sprites without a client rebuild (asset just has to exist on the
server).

## Client rendering

New module `crates/gameclient/src/entities.rs`. Mirrors `OtherPlayerSprite`:

- `MobileEntities` resource, populated from polled `/entities`.
- Spawn `Sprite + MobileEntityMarker` for new ids; despawn for ids that drop
  out of viewport.
- Per frame: interpolate translation toward the most recent polled tile
  position; advance walk-cycle atlas frame; flip x by `facing`.
- z = 1.5 (same layer as static monsters).

Sprite asset registry: generalize the existing `monster_files` map in
`tilemap.rs` to a per-sprite-name lookup that loads from `assets/sprites/{type}/`
where `type` is `monsters` / `npcs` / `animals`.

## Trigger / interaction surface

### Combat (monsters)
Server starts combat exactly like a static monster encounter: synthetic
`event_id = "mobile_monster:{entity.id}"`, `init_combat()` from the existing
combat module. Victory removes the entity from the live world and starts the
respawn timer if configured.

### Dialogue / trade (NPCs)
On adjacency (Chebyshev distance ≤ 1), server triggers the NPC's `event_id`
for that player. Reuses existing event-activation logic; the event JSON carries
the dialogue lines / outcomes / shop config.

### New trigger condition

```rust
TriggerCondition::NearEntity { entity_id: String, radius: u32 }
```

Lets quest events fire when the player approaches a specific entity ("Talk to
the wandering merchant"). Composable with `All` / `Any` / `Not` like the rest.

## Multiplayer rules

| Situation | Behavior |
|---|---|
| Two players see the same entity | Yes — single position, both clients render it identically. |
| Player A engages entity in combat | Entity vanishes from B's view for the duration; reappears if A loses, stays gone if A wins. |
| Two players adjacent to same NPC | Independent dialogue per player (event state is already per-player). |
| Player walks onto an entity already in combat with someone else | Entity invisible to them; no double-combat. |

## State persistence

- Authored data: re-read from JSON on every server startup. Content updates
  don't need a save wipe.
- Runtime data (position, alive/dead, respawn timer, behavior state): saved
  in `dev_state.json` under `mobile_entities`.
- If an authored entity id disappears from JSON (removed from the file), its
  saved state is pruned on load (same way item-id pruning works for
  inventories).

## Asset pipeline

Each new entity type needs a 4-direction walking sprite-sheet (16 × 16 ×
4 frames × 4 directions = 64 px wide × 64 px tall PNG, atlas row = facing).
Phase-1 placeholder strategy: reuse existing static-monster sprites
(Slime, Wolf, etc.) so we can ship the system before any new art lands.

When real art arrives, drop it in `assets/sprites/monsters/` (or
`/npcs/` / `/animals/`) and reference by `sprite` name in JSON.

## Open decisions to revisit

- **Hazards** (rolling rocks, area damage without combat) — Phase 3.
- **Mounts / pets** (entities tied to a player) — out of scope.
- **Day/night-aware schedules** (shopkeepers sleep at night) — Phase 3.
- **Entity-entity interactions** (guard chases thief NPC) — Phase 4, probably
  not needed.
- **Per-entity-per-player dialogue memory** (NPC remembers our last
  conversation) — would extend the event-state model; not MVP.

## Implementation order

1. `questlib::mobile_entity` module: types + JSON loading + tests.
2. `gamemaster::mobile_entity` module: tick loop with Wander only.
3. `GET /entities` endpoint.
4. Client `entities.rs` module: poll + render + interpolate.
5. Add Patrol behavior.
6. Combat + dialogue contact triggers.
7. Save / load wiring.
8. `seed12345_entities.json`: one wolf, one wandering NPC.
9. Smoke-test on dev, ship.

Each step is mostly isolated; can be PR'd separately.
