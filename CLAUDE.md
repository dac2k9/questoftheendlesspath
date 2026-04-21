# CLAUDE.md — 10000m To Target

## Development Philosophy

This project is spec-driven. This file (CLAUDE.md) is the source of truth for how the program behaves — architecture, protocols, API surface, controls, game rules. Behavior changes are documented here as part of the same task that introduces them; implementation details that are too granular for this file live as comments in code. Bug fixes and small refactors don't require a preamble update, but if the fix changes user-visible behavior, the relevant section here changes too.

Simplicity is a hard requirement. If something feels complex, stop and simplify before continuing. Prefer deleting code over adding abstractions. Prefer the browser's built-in behavior over reimplementing it in JavaScript. When in doubt, the shorter code wins.

Look broadly before implementing. Every new feature is an opportunity to simplify what's already there. Before writing new code, check existing structs, queries, and patterns — consolidate, remove dead code, and unify duplicates. Don't add a new thing next to an old thing that does almost the same job (e.g. a second click handler that duplicates the first with a small conditional).

Propose before non-trivial work. For anything beyond a bug fix or small refactor, sketch the approach in one or two short paragraphs and get agreement before writing code. Include what you'd touch, what you'd skip, and any alternatives you considered. Small fixes and obvious cleanups can stay fast — but when a task could plausibly be done two different ways, discuss first.

CLAUDE.md is updated as part of every task. Any change to behavior, architecture, protocol, API surface, controls, or roadmap phase is reflected here before the commit. A task isn't done until CLAUDE.md agrees with the code. This file is what future conversations read first — if it's wrong, everything built on it is wrong.

## What This Is

A cooperative multiplayer treadmill RPG. Players walk on UREVO CyberPad treadmills and their characters move through a procedurally generated fantasy world. Built in Rust.

## Architecture

```
CyberPad treadmill → Walker (Rust/BLE) → Dev Server (:3001) → Game Master (Rust) → Browser (Bevy WASM)
```

### Crates

- `questlib` — Shared library: FTMS parser, map generator, event system, fog, leveling, route logic
- `walker` — BLE treadmill reader: FTMS + UREVO proprietary protocol, step tracking, activity detection
- `gamemaster` — Game logic: tick loop, event triggers, route advancement, fog updates, dev HTTP server
- `gameclient` — Browser game: Bevy 0.15 compiled to WASM, tilemap rendering, HUD, dialogue

### Key Files

- `crates/questlib/src/mapgen.rs` — Seeded procedural world generator (terrain + POIs + roads)
- `crates/questlib/src/events/` — Event system: typed kinds, composable triggers, state machine
- `crates/questlib/src/fog.rs` — Fog of war bitfield (base64 encoded)
- `crates/questlib/src/leveling.rs` — Level from distance walked (cubic scaling)
- `crates/questlib/src/route.rs` — Route advancement, tile costs
- `crates/gamemaster/src/tick.rs` — Main game tick (3s interval)
- `crates/gamemaster/src/devserver.rs` — Local HTTP dev server (replaces Supabase)
- `crates/gameclient/src/terrain/tilemap.rs` — World rendering, pathfinding, camera, player sprite
- `crates/gameclient/src/hud/` — Gold counter, speed, distance, floating text, journal panel (J), minimap (bottom-right)
- `crates/gameclient/src/dialogue/` — JRPG dialogue box, notification banners, event polling
- `adventures/seed12345_events.json` — Event definitions for the default world
  (seed set via `MAP_SEED`, events file via `EVENTS_PATH`)

## Build & Run

The Game Master serves BOTH the API and the WASM client on port 3001,
so no separate static server is needed.

```bash
# Game Master + static file server (Terminal 1)
cd /Users/dac/src/walk && cargo run -p gamemaster

# Build WASM client (Terminal 2, re-run after client code changes)
cd crates/gameclient && cargo build --target wasm32-unknown-unknown
wasm-bindgen target/wasm32-unknown-unknown/debug/gameclient.wasm --out-dir web --target web --no-typescript

# Open: http://localhost:3001/
```

The `walker/` crate (direct BLE treadmill reader) is excluded from the
workspace — the Walker bridge in gamemaster connects to walker.akerud.se
over WebSocket instead. No Bluetooth permission needed on this machine.

### Debug Walking (no treadmill needed)
After joining via the title screen, grab your player_id from
`GET http://localhost:3001/players` and plug it in:
```bash
PID="<your-player-id>"

# Simulate 3 km/h walking
while true; do curl -sX POST http://localhost:3001/debug_walk \
  -H 'Content-Type: application/json' \
  -d "{\"player_id\":\"$PID\",\"speed\":3.0}"; sleep 3; done

# Stop
curl -sX POST http://localhost:3001/debug_walk \
  -H 'Content-Type: application/json' \
  -d "{\"player_id\":\"$PID\",\"speed\":0}"
```

### Reset Game State
```bash
rm dev_state.json  # Local dev only. On Render, reset the persistent
                   # disk or wipe the file via the service shell.
```

`SAVE_PATH` env var controls where `dev_state.json` lives (default: CWD
for local, `/app/dev_state.json` in Docker, set to `/data/dev_state.json`
on Render with a persistent disk mounted at `/data`).

## WASM Build Notes

- gameclient is **excluded** from the workspace (WASM-only, separate target)
- Always build from `crates/gameclient/` directory: `cd crates/gameclient && cargo build --target wasm32-unknown-unknown`
- Building from workspace root fails (mio/tokio don't compile for WASM)
- Bump `?v=N` in index.html after each build to bust browser cache
- `getrandom` needs both `js` (v0.2) and `wasm_js` (v0.3) features
- `webgl2` feature required for Bevy in browser (WebGPU not supported everywhere)
- `AssetPlugin { meta_check: AssetMetaCheck::Never }` — required for WASM asset loading

## Walker bridge (treadmill → gamemaster)

The gamemaster opens a WebSocket to `wss://walker.akerud.se/ws/live/<walker_uuid>`
per player and translates Walker's segment updates into `is_walking` /
`current_speed_kmh` / `total_distance_m` on `DevPlayerState`.

This is the **only** path that writes treadmill-derived data into game state.
The legacy `POST /walker_update` HTTP endpoint — which the excluded `walker/`
crate used to call with client-supplied `distance` — was removed: it was
dead code AND would have let any client write arbitrary distance to any
player. Trust boundary is now: clients submit geometry (`/set_route`) and
admin intents (`/admin/*`); the server owns positions, distances, and state.

Resilience:

- **Active keepalive.** The bridge sends a WebSocket PING every 30s. If no
  inbound frame (text / pong / ping / binary) arrives for 60s, the bridge
  considers the socket half-dead, returns an error, and the retry loop in
  `ensure_bridge` reconnects. Catches "Walker's side went away but our TCP
  socket thinks it's alive" without relying on OS TCP keepalive (which
  defaults to hours).
- **Short-run failure cap.** `ensure_bridge`'s retry loop tracks *consecutive
  short runs* (<30s before error) — normal long-lived sessions that hit a
  clean disconnect don't count. Gives up only after 120 short runs in a
  row (~10 min of genuine connect failures).
- **Close-frame → reconnect.** If Walker sends a WS Close, we return Err
  too; same retry path.

Diagnostic / recovery:

- `WALKER_BRIDGE_TRACE=1` env var logs every incoming message, parse
  failures, rate-limit drops, and outbound pings. Off by default.
- `POST /admin/respawn_bridge {"player_id":"…"}` removes the player from
  `bridged_players` and calls `ensure_bridge` to spawn a fresh connection
  without a redeploy. Gated on `ADMIN_TOKEN`.

## UREVO CyberPad BLE

- Device name: `URTM051`
- **FTMS** (standard): Service 0x1826, Treadmill Data 0x2ACD — speed, distance, incline
- **UREVO proprietary**: Notify 0xFFF1, Write 0xFFF2
  - Activate: write `02 51 0B 03` to 0xFFF2
  - 19-byte packets: status[2], speed_mph[3], duration[5-6], distance_km[7-8], calories[9-10], **steps[11-12]**
- **Incline quirk**: Setting incline while running stops the treadmill. Workaround: resume + restore speed after 1.5s
- macOS: Terminal needs Bluetooth permission (System Settings > Privacy > Bluetooth)

## Game Design

### Events
- Defined as JSON in `adventures/seed42_events.json`
- Types: NpcDialogue, Treasure, RandomEncounter, Quest, Shop, Boss, StoryBeat, EnvironmentalEffect
- Triggers: AtPoi, AtTile, InBiome, DistanceWalked, EventCompleted, HasItem, RandomInBiome, All, Any
- `requires_browser: true` events pause map progress until dismissed
- Auto-complete events (treasure, story) apply outcomes immediately + push notifications

### POIs
- Generated by `WorldMap::generate(seed)` — deterministic from seed
- Types: Town, Village, Ruins, Dungeon, Cave, Cabin, Shrine, Tower, Camp, Port
- `poi_at()` matches within 1 tile of POI center
- POI tiles are set to Road ground (cheap traversal)
- Player must deliberately click on/near POI to walk there — no auto-snapping
- **Visual markers on the map:** `PoiType::Cave` renders an `Overlay::CaveEntrance`
  sprite (tile atlas index 43); all other POI types render `Overlay::Village`
  (index 84, well). Hovering with TAB still shows the exact POI type as text.
  Cave entrances are visually distinct so players can find them without
  needing to hover.

### Movement
- Player clicks tiles to plan route (A* pathfinding overworld, BFS interior)
- Walker sends distance deltas every 2s
- Game Master advances player along route based on accumulated distance
- Tile costs: Road 20m, Grass 40m, Sand 50m, Forest 70m, Snow 70m, Swamp 100m, Mountain 120m
- Character interpolates smoothly between tiles based on speed

### Trust boundary: client submits geometry, server owns distance
- `/set_route` takes ONLY the route waypoints. The server computes
  `route_meters_walked` by finding the player's current tile in the new
  route and summing tile costs up to that index (or 0 if not found).
- The client never tells the server "I have moved X meters." That lets the
  server stay authoritative on position + prevents the multi-click
  teleport bug where the client's interpolated meters got handed back.
- Same rule applies inside interiors (flat `floor_cost_m` per tile).
- Forward-only: browser never moves current_index backwards

### Activity Detection
- UREVO step data detects if user is actually walking vs belt just running
- No steps for 5s → idle (0 speed, 0 distance sent)
- Prevents cheating by running belt without walking

### Fog of War
- Revealed in 5-tile radius around player
- Stored as base64-encoded bitfield (1000 bytes for 100x80 map)
- Persisted in save file, restored on Game Master restart
- Fogged tiles show "???" on hover, can't click to plan route

### Leveling
- Walking distance = XP. Formula: `3 * N^3 + 70 * N` meters for level N
- Lvl 2: 164m, Lvl 10: 3.7km, Lvl 30: 83km

### Sound effects
- 8-bit square-wave blips synthesized on-the-fly in `crates/gameclient/src/sfx.rs`.
  No audio assets shipped — each sound is a short note sequence built with
  the browser's Web Audio API (`AudioContext` + `OscillatorNode` + `GainNode`).
- Four events, all detected client-side from state deltas:
  - **GoldGained** (positive gold jump): C5 → E5 chirp
  - **RouteArrived** (planned route went empty): E5 → C5 soft descending
  - **LevelUp** (character level increased): C5 → E5 → G5 triad
  - **CombatVictory** (combat went active → inactive): G4 → C5 → E5 → G5 fanfare
- SFX volume multiplies the music master volume, so the existing mute /
  slider controls SFX too.
- To replace synthesized sounds with sampled MP3s later: swap the body of
  `sfx::play_sfx` to use `HtmlAudioElement::new_with_src` per `SfxKind`.
- +15 HP, +3 Attack, +2 Defense per level

## Controls (Browser)

- **Left click** — plan a new route to the clicked tile (replaces any current route)
- **Shift + Left click** — extend the current route with another destination
- **Right click drag** — pan camera
- **Scroll wheel** — zoom (smooth, 500ms easeout)
- **ESC** — clear planned route
- **TAB (hold)** — show your name tag, other players' name tags, tile info on hover, POI labels
- **I** — toggle inventory panel (right side)
- **J** — toggle journal panel ("story so far" of completed events)
- **X** — dismiss notification banner
- **Enter/Space/Click** — advance dialogue
- **F3** — debug menu (FPS, fog toggle, POI toggle)

## Dev Server API (localhost:3001)

- `GET /players` — all player states
- `POST /set_route` — `{"player_id":"...","route":"[[x,y],...]"}`
- `POST /debug_walk` — `{"player_id":"...","speed":3.0}` (simulate walking)
- `GET /events/active?player_id=X` — events currently visible to this player
- `POST /events/{id}/complete` — `{"player_id":"..."}` required; mark event completed
- `GET /journal?player_id=X` — completed events for this player, rendered by
  the Journal panel (J). Skips shops and environmental effects. Each entry is
  `{id, name, description, kind}` in completion order.
- `GET /notifications?player_id=X` — fetch + clear this player's pending notifications
- `POST /heartbeat` — browser presence (no-op in dev)
- `GET /leaderboard` — proxy to walker.akerud.se leaderboard (bypasses CORS)

### Interior spaces (caves / dungeons / castles)

Design principle: **one abstraction for every walk-into-something space**.
Whether it's a single-chamber cave, a shortcut tunnel connecting two
overworld tiles, or a multi-room castle, they're all `InteriorMap`s
connected by `Portal`s. A portal's `destination` is either an overworld
tile or another interior — so a shortcut is just a cave with two
portals pointing to different overworld tiles, and a castle is a set
of interiors linked by portals to each other.

#### Data model (`questlib::interior`)

- `Location::Overworld | Location::Interior { id }` — on `DevPlayerState.location`
- `InteriorMap { id, name, width, height, tiles, portals, chests, floor_cost_m }`
- `InteriorTile::Wall | Floor`
- `Portal { x, y, destination, label }` with `PortalDest::Overworld { x, y } | Interior { id, x, y }`
- Compound chest key `"<interior_id>:chest:<idx>"` in `player.opened_chests`
  (overworld chest keys are still plain numeric ids)

Interiors load from `adventures/interiors/*.json` at startup into an
`Arc<HashMap<String, InteriorMap>>` shared with the devserver and tick
loop. `INTERIORS_DIR` env var overrides the path.

#### Endpoints

- `GET /interior?id=X` — full `InteriorMap` (tiles + portals + chests)
- `POST /enter_interior` — `{"player_id":"...","interior_id":"whispering_cave","spawn":[1,1]}`
- `POST /use_portal` — `{"player_id":"..."}` — takes the portal at the
  player's current interior tile (falls back to `overworld_return` if off-portal)

#### Runtime

`DevPlayerState` gains three fields (all `#[serde(default)]`, so existing
saves load cleanly):
- `location: Location` — default `Overworld`
- `overworld_return: Option<(i32,i32)>` — tile to pop back to
- `interior_fog: HashMap<String, String>` — base64 fog bitfield per interior id

The overworld tick (`tick::run_tick_dev`) short-circuits for any player
whose `location.interior_id().is_some()`. Interior players are handled
separately by `interior::run_interior_tick`, which mirrors the essential
overworld mechanics (walker-derived delta → route advancement → fog
reveal → chest open) against `interior.tiles`.

#### Status & roadmap

**Phase 1 (shipped)** — server-only MVP
- [x] Types in `questlib::interior`
- [x] Loading + tick + endpoints in `gamemaster::interior`
- [x] One hand-authored cave (`adventures/interiors/whispering_cave.json`,
      16×12, 1 chest, 1 exit portal)
- [x] Save-safe schema additions; fog persistence per interior
- [x] Route planning + walking inside; chest open gives +50 gold

**Phase 2 (shipped)** — client tilemap swap
- [x] Client watches `MyPlayerState.location`; on change, hides overworld sprites (MapSprite/FogSprite) and fetches `/interior?id=X`
- [x] Interior rendered as colored quads (walls, floor, portal, chest) — proper dark tileset is a Phase 3 polish item
- [x] Click on a walkable interior tile: BFS through the interior grid → `POST /set_route`
- [x] Click on a portal tile: routes the player to it (auto-use_portal triggers when they arrive — see Phase 3a)
- [x] Other players filtered by co-location so no cross-scene overlap
- [x] HUD shows `⟐ <Interior Name>` while inside

**Phase 3a (shipped)** — POI integration
- [x] New `EventKind::CaveEntrance { interior_id, spawn_x, spawn_y, flavor }` wired into the tick loop before combat/boss gating; no dialog UI, just teleport + flavor notification
- [x] Attached at POI 12 (Mountain Cave) → Whispering Cave
- [x] `PortalDest::OverworldReturn` variant + `enter_interior` saves the player's `prev_tile` as `overworld_return`, so exits land off the entrance POI (no re-trigger loop)
- [x] Auto-use_portal: `run_interior_tick` transitions the player when a route step lands on a portal tile for the first time that tick

**Phase 3b (shipped)** — monsters inside interiors
- [x] `InteriorMonster { x, y, monster_type, difficulty }` field on `InteriorMap`
- [x] `monster_at`/`monster_key`/`monster_combat_event_id` helpers in `questlib::interior`
- [x] `run_interior_tick` starts combat when the player steps onto an un-defeated
      monster tile; uses the existing `server_combat::start_combat` with a
      synthetic event_id `"interior_monster:<id>:<idx>"`
- [x] Victory handler in `tick.rs` parses that event_id and awards the same
      difficulty-scaled gold + item drop as overworld monsters, pushes
      `<id>:monster:<idx>` onto `defeated_monsters`
- [x] Client renders monsters as red quads tagged with `InteriorMonsterMarker`;
      `sync_monster_visibility` hides defeated ones by compound key
- [x] Interior-tick freezes movement while in combat (same rule as overworld)
- [x] Whispering Cave seeded with a slime (diff 1) and a skeleton soldier (diff 3)

**Phase 3c (shipped)** — per-chest loot tables
- [x] `ChestLoot { gold, items }` + `InteriorChest { x, y, loot }` in `questlib::interior`
- [x] `InteriorMap.chests` is now `Vec<InteriorChest>` instead of `Vec<(usize, usize)>`
- [x] `run_interior_tick` grants `loot.gold` + each `loot.items` entry via `add_item` when the player steps on a chest; notification lists the gold plus item display names
- [x] Whispering Cave chest upgraded to `{ gold: 80, items: ["health_potion", "torch"] }`
- [x] Monsters still use the overworld difficulty-scaled drop table — separate concern

**Phase 3d (shipped)** — shortcut caves + torches
- [x] `Portal.unlock_event_id: Option<String>` — portal refuses transition
      until the named event is in the player's `completed_events`
- [x] `PortalTransitionResult::Locked { label }` + `/use_portal` returns
      403 when locked; auto-portal-on-arrival pushes a "sealed from this
      side" notification instead of transitioning
- [x] `EventKind::CaveEntrance.consume_on_entry: Option<String>` — if set
      (e.g. `"torch"`), one is required in inventory and consumed on entry.
      Missing the item: notify + skip, no transition, no progress recorded
- [x] CaveEntrance events re-trigger even after completion (filter in
      `tick.rs` allows repeated entry) — walk back to a cave mouth and
      re-enter with another torch
- [x] **Stone Tunnel** shortcut cave (22×12) with two portals:
      north → overworld (37, 58), south → overworld (66, 63);
      chest in the middle with 120 gold + Greater Health Potion
- [x] Both stone-tunnel entrances + the whispering-cave entrance require
      `consume_on_entry: "torch"`. Torches sold at Forest Town + Midland
      Village shops for 20g
- [x] Client dims locked portals (orange) vs. unlocked (teal); the color
      re-syncs the tick the unlock event lands in `completed_events`

**Phase 3e (shipped)** — weighted chest rolls
- [x] `ChestLoot.rolls: Vec<LootRoll { item_id, chance }>` alongside the
      guaranteed `gold`+`items`. Each roll is an independent flip; chance
      is clamped to `[0.0, 1.0]`.
- [x] `questlib::interior::roll_rng(player_id, chest_key, item_id)` is a
      deterministic hash → `[0.0, 1.0)`. Same inputs → same output, so
      rerolls by reload are impossible and tests are reproducible.
- [x] `evaluate_rolls` + `roll_rng` unit tests in `questlib::interior`.
- [x] `run_interior_tick` grants guaranteed `items` then appends the
      results of `evaluate_rolls` to the "Opened a hidden chest!"
      notification line.
- [x] Whispering Cave chest: +50 % Health Potion.
- [x] Stone Tunnel chest: +25 % Iron Sword, +40 % Torch (refill chance).

**Phase 3 remaining**
- [ ] Real dark tileset (replace colored quads in `terrain/interior.rs`,
      including proper monster sprites reusing the overworld loader)
- [ ] Procedural cave generator keyed off POI id

**Phase 4 (planned)** — character paper-doll inventory
- [ ] Rework the inventory panel: character silhouette with slot outlines
      (weapon, armor, accessory, feet, toe rings, and a reserved head
      slot for later)
- [ ] Drag-and-drop: press an inventory item → drag → drop on a slot to
      equip. Drag equipped item back onto the list to unequip.
- [ ] Visual feedback: ghost image following the cursor, valid drop
      zones highlight, invalid (wrong slot for that item) dim.
- [ ] Keep click-to-equip as a fallback for accessibility / mobile.
- [ ] Open design questions: add a head slot now or wait? show slot
      icons or generic outlines? should "unequip to full inventory"
      reject the drop with a tooltip or swap-drop the oldest?

**Phase 5 (planned)** — player co-location features
- [ ] When two players share a tile, reveal each other's fog maps to each
      other (one-time merge at the moment of overlap, or continuous while
      co-located — TBD)
- [ ] Trade UI: both players on the same tile can open a trade window and
      exchange items / gold with explicit both-accept confirmation
- [ ] Design open questions: does co-location count only on overworld, or
      also inside the same interior? Do the maps merge both directions or
      one-way? What's the anti-grief story (someone accepting a trade
      without consent)? Discuss before implementing.

**Phase 4 — castles** (speculative)
- [ ] Multi-room interiors (several `InteriorMap`s linked by `PortalDest::Interior`)
- [ ] Richer interior tilesets / boss lairs / lore rooms

### Admin endpoints

Gated on both the `ADMIN_TOKEN` env var (must be non-empty) and an
`X-Admin-Token: <value>` header. Use for one-off fixes: giving an item,
resetting an event's global status, granting/revoking per-player completion.

```bash
TOKEN=...  # same value as ADMIN_TOKEN on the server
BASE=https://questoftheendlesspath-latest.onrender.com
curl -s -X POST $BASE/admin/give_item \
  -H 'Content-Type: application/json' -H "X-Admin-Token: $TOKEN" \
  -d '{"player_id":"<uuid>","item_id":"seven_league_boots","quantity":1}'
curl -s -X POST $BASE/admin/reset_event \
  -H 'Content-Type: application/json' -H "X-Admin-Token: $TOKEN" \
  -d '{"event_id":"find_traveler","status":"pending"}'
curl -s -X POST $BASE/admin/revoke_completion \
  -H 'Content-Type: application/json' -H "X-Admin-Token: $TOKEN" \
  -d '{"player_id":"<uuid>","event_id":"find_traveler"}'
```

## State Persistence

- `dev_state.json` — auto-saved every 30s by Game Master, and on SIGTERM
- Contains: player positions, gold, inventory, fog, per-event status
- Versioned (`SaveData.version`); `migrate_save` handles older versions
- Item ids that disappear from the catalog are pruned from inventory/equipment on load
- Event definitions are re-read fresh from `EVENTS_PATH` every startup — only per-event status is carried over, so content updates (shops, triggers, outcomes) don't require wiping the save
- Delete to reset locally: `rm dev_state.json`

## TODO

- Persistent state on Render (free tier wipes dev_state.json on deploy)
- LLM adventure skill (`/adventure` generates events from POI JSON)
- Real auth (currently /notifications only does a sanity check; anyone with a
  known player_id can poll that player's queue)
- Delete or revive the excluded `walker` crate (currently dead-on-disk)
- Shared-goal widgets / team stats in HUD to reinforce co-op
