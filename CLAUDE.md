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
- **ALSO** bump `CLIENT_VERSION` in `crates/gameclient/src/version.rs` to
  the same number. The server exposes that `?v=N` via `GET /version`;
  the client polls it every 60 s and if server version > CLIENT_VERSION,
  a "New version available" banner with a Refresh button appears. Missing
  bump = clients never realize they're stale.
- `getrandom` needs both `js` (v0.2) and `wasm_js` (v0.3) features
- `webgl2` feature required for Bevy in browser (WebGPU not supported everywhere)
- `AssetPlugin { meta_check: AssetMetaCheck::Never }` — required for WASM asset loading

## Deploy

Production runs on Render at https://questoftheendlesspath-latest.onrender.com.
The Render service is **Image-type** — it pulls a pre-built Docker image
from GHCR rather than building from source.

Pipeline (fully automatic on push to `main`):

1. `git push` to `main`
2. `.github/workflows/docker.yml` builds the multi-stage Dockerfile and
   pushes the image to GHCR with `:latest` and `:<sha>` tags
3. The same workflow then `curl`s Render's Deploy Hook URL (stored as the
   `RENDER_DEPLOY_HOOK` GitHub Actions secret), which kicks off a deploy
   that pulls the new `:latest`
4. Render builds in ~2-4 min; the version banner on already-loaded clients
   notices and shows the Refresh button after the next `/version` poll

If the Deploy Hook step ever stops working, the workflow's curl step is
gated on the secret being non-empty, so the build itself still succeeds —
fall back to **Render dashboard → Manual Deploy → Deploy latest reference**
in the meantime, then check why the secret got cleared.

## Walker bridge (treadmill → gamemaster)

The gamemaster opens a WebSocket to `wss://walker.akerud.se/ws/live/<walker_uuid>`
per player and translates Walker's segment updates into `is_walking` /
`current_speed_kmh` / `total_distance_m` on `DevPlayerState`.

**Segment semantics (important).** A *segment* in the walker feed is a
continuous period where state is constant — same walking/idle, speed,
incline. Any state change closes the current segment and opens a new
one. `segment.distance_m` only reflects movement *within that segment*,
not cumulative across the day. The bridge tracks `segment.started_at`
and **re-baselines `last_distance` whenever it changes**; without that,
naively computing `current.distance_m − previous.distance_m` across a
segment boundary produces a phantom delta equal to the full new
segment's distance, inflating `total_distance_m` by anywhere from a
few meters to a few km in a single tick (and skipping multiple levels).
The same reset happens on initial connect / reconnect. There is also a
50-m defense-in-depth cap on per-message deltas after that.

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
- Triggers: AtPoi, AtTile, InBiome, DistanceWalked, EventCompleted, HasItem, RandomInBiome, All, Any, Not
- `Not { inner }` inverts a condition (e.g. `at_poi 20 + not has_item warm_cloak` to fire a "missing prereq" notification when the player walks onto a gated POI without the gate item). Field is `inner`, not `condition`, to avoid colliding with the serde tag.
- `requires_browser: true` events pause map progress until dismissed
- Auto-complete events (treasure, story) apply outcomes immediately + push notifications
- **Climactic-boss scaling overrides.** `EventKind::Boss` accepts optional `hp_per_level` / `atk_per_level` / `def_per_level` fields (default 20 / 2 / 0). They tune how aggressively the fight scales with player level when `scales_with_player: true`. The Frost Lord uses `25 / 3 / 1` so player damage growth doesn't outrun boss HP growth — without `def_per_level > 0`, the player's +3 ATK/level eventually trivializes any final boss. Other bosses keep the defaults via `unwrap_or`, so existing saves and content load unchanged.

### POIs
- Generated by `WorldMap::generate(seed)` — deterministic from seed
- Types: Town, Village, Ruins, Dungeon, Cave, Cabin, Shrine, Tower, Camp, Port
- `poi_at()` matches within 1 tile of POI center
- POI tiles are set to Road ground (cheap traversal)
- Player must deliberately click on/near POI to walk there — no auto-snapping
- **Visual markers on the map:** POI types with a custom PNG in
  `crates/gameclient/assets/poi/` render an illustrated landmark
  sprite (Town, Village, Cave, Cabin, Shrine currently). Types without
  custom art fall back to the `Overlay::Village` tile-atlas marker
  (Ruins, Dungeon, Tower, Camp, Port). Mapping lives in
  `tilemap::poi_sprite_path`, returning `(path, tile_size)` where
  tile_size is 1–3 tiles — small landmarks use 1, iconic ones
  (castles, fortresses) use 3. Widening beyond 3 requires expanding
  the 3×3 overlay-clearing pass in `terrain/world.rs`. Hovering with
  TAB still shows the exact POI type as text.

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

### Mobile entities

Autonomous moving NPCs / monsters / animals. Defined in
`adventures/seed{N}_entities.json` (override path via `ENTITIES_PATH`
env var), one entity per object with `id` / `kind` / `sprite` / `spawn`
/ `behavior` / `on_contact` / optional `respawn_after_secs`.

Behaviors (Phase 1):
- **Wander**: random walk within `radius` tiles of the spawn point.
- **Patrol**: loop through a list of `waypoints`; `loop_mode` is
  `wrap` or `bounce`.

Contact actions:
- **Combat**: when an entity ends a server tick on a player's tile,
  combat starts via the existing `init_combat` path. Synthetic
  `event_id` is `mobile_monster:{entity_id}`. Victory marks the entity
  dead and schedules respawn (configurable per entity).
- **Dialogue / Trade**: when a player enters Chebyshev distance ≤ 1
  for the first time, push a notification with the entity's `name`.
  Full dialogue UI activation is Phase 2.

Server: `gamemaster::mobile_entity` owns the tick loop, contact phase,
and JSON loader. Client: `gameclient::entities` polls
`/entities?player_id=X` every 1 s, renders + interpolates sprites
keyed off the `sprite` registry name (currently mapped to existing
monster atlases — `slime`, `club_goblin`, `skeleton_soldier`, etc.).
Save-state preserves runtime state (position, alive, respawn timer)
under `mobile_entities`; authored bits reload fresh from JSON every
startup.

Quirks worth knowing:

- **`/entities` viewport filter is currently OFF** (returns every alive
  entity regardless of distance). Was a 20-tile Chebyshev radius for
  bandwidth — turned off until the world has enough entities to
  matter. Easy flip in `devserver.rs::handle_request` (the
  `VIEW_RADIUS` const).
- **Auto-reset on spawn-coord change.** `MobileEntityState.spawn`
  mirrors `def.spawn` at creation; `ensure_states` re-inits any
  entity whose saved spawn no longer matches the JSON, so editing
  `seed{N}_entities.json` and restarting moves the entity to its
  new home automatically. No `dev_state.json` surgery required.
- **Combat is one-player-per-entity.** `check_contacts` skips an
  entity entirely when `shared_combat` already has its event_id,
  and breaks after binding the first matching player on the tile.
  Without these guards two players standing on the same tile
  ping-ponged combat-state inserts each tick.
- **Client snaps on big position jumps** (> 2 tiles) instead of
  smoothly interpolating across them — handles auto-respawn and
  spawn-coord edits cleanly without a long visible "leap".

Full design spec: `adventures/MOBILE_ENTITIES.md` (Phase 2/3 sections
still apply).

### Adventures (chapters)

The game is split into named "adventures" (frost_quest, chaos, …),
each with its own seed, events JSON, mobile entities, and (eventually)
interiors directory. Adventures live in `gamemaster::adventure`:
`presets()` returns the registered list, `load_bundle()` builds one,
and the server loads ALL registered bundles at startup.

Each player carries an `adventure_id` (`DevPlayerState.adventure_id`,
`#[serde(default = "frost_quest")]`). The tick loop iterates bundles
and ticks only the players whose `adventure_id` matches that bundle —
overworld + interior + mobile-entity ticks all scope per-bundle.
Players in different adventures don't share events, monsters, or
combat targets; they DO share `/players` (the client filters by
adventure to decide visibility, similar to overworld vs. interior).

`POST /start_new_adventure {player_id, adventure_id}` resets the
player's level / gold / inventory / equipment / route / fog /
opened_chests / defeated_monsters / completed_events. **Boons are
preserved** (the whole point of the meta-progression system), along
with id / name / champion / walker_uuid. The in-game adventure menu
button (top-right) posts to this endpoint and then reloads the page.

**One-way trip.** Each `AdventurePreset` declares a
`completion_event_id` (frost_quest: `tower_20`, chaos: `chaos_starstone_avatar`).
`GET /adventures?player_id=X` returns
`{ current, current_completed, available[] }`. The `available` list is
empty until the player's current adventure's `completion_event_id` is
in their `completed_events` — so the in-game switcher button only
appears once they've earned the right to advance. After the switch,
the new adventure's button immediately hides again (current changed
to chaos, which they haven't completed). Adventures the player has
already finished are filtered out, so you never see a "go back to
frost_quest" option. Client polls `/adventures` every 5 s; the button
itself is always spawned but `Visibility::Hidden` flips based on
`available.is_empty()`.

**Per-adventure world size.** Each `AdventurePreset` carries
`map_width` / `map_height` (frost_quest: 100×80, chaos: 200×160 —
4× area). `/join` and `/start_new_adventure` return these alongside
`map_seed`; the client stores them in `GameSession` and calls
`WorldGrid::from_seed_with_dims` on enter-game. The old compile-time
`WORLD_W` / `WORLD_H` constants are now runtime values published
via `set_world_dims` (atomic statics readable via `world_w()` /
`world_h()`); call sites that don't have direct access to the
`WorldGrid` Resource read those instead. Chaos's authored POI
coordinates in `seed99999_pois.json` are tuned for the 200×160
layout (castles in each outer quadrant, gates between camp and
castles, camp + spire central). Save migration (`migrate_to_bundle_dims`
in `main.rs`) resets fog when a player's saved bitfield doesn't decode
at the bundle's dims and clamps OOB positions to world centre.

### Boons (meta-progression)

Permanent rewards earned by defeating climactic bosses. Boons survive
across adventures even though level / gold / inventory all reset on
each new run — they're the only thing that compounds across runs.

Source of truth: `questlib::boons` (static catalog + effect helpers).
Per-player owned ids live on `DevPlayerState.boons: Vec<String>`,
which is in `#[serde(default)]` so existing saves load.

Flow:
1. Boss event has `grants_boon: true` (default false). On victory the
   server sets `pending_boon_choice` on each participant via
   `tick::queue_boon_choice` — three deterministic ids picked from
   `pick_choices(seed=hash(player_id, event_id), n=3, owned)`.
2. Client polls `MyPlayerState.pending_boon_choice`, opens the boon
   picker modal (`gameclient::boon_picker`), shows three cards.
3. Click → `POST /select_boon {player_id, boon_id}` → server validates
   and pushes to `player.boons`, clears `pending_boon_choice`.
4. Effect helpers in `questlib::boons` (`speed_multiplier`,
   `gold_multiplier`, `biome_cost_multiplier`, etc.) are folded into
   the relevant compute sites in tick.rs and `/forge_upgrade`.

V1 catalog (9 boons): Swift Boots (+5% walk), Trailblazer
(forest/swamp/snow −20% cost), Roadwise (roads extra −25%), Sprint
(+20% for first 1 km of session), Goldfinger (+10% gold), Wealthy
Start (+500 gold per adventure), Treasure Sense (chests on minimap
within 10 tiles through fog — wired client-side later), Forge
Discount (−25% upgrade cost), Cartographer (+1 fog reveal radius).

**Adventure-scoped boons.** `EventOutcome::AdventureBoon { boon_id }`
appends to `player.adventure_boons[adventure_id]` (a
`HashMap<String, Vec<String>>`) instead of the permanent `player.boons`.
They only apply while the player is in that adventure — switching away
and coming back resumes them. Used by the chaos arc to attach
themed bonuses to each boss drop without inflating the permanent
catalogue. Chaos boons currently:
- Frost Queen → Frostproof (Snow tiles −50%)
- Lord of Flame → Forge-Tempered (forge cost −50%)
- Hierophant of Shadow → Voidsight (fog reveal +2)
- Stormbinder → Lightning-Footed (roads −50%)
- Echo of the First Cut (climax) → Starstone Awakened (+50% gold, fog reveal +3)

**Chaos arc final act.** After all four castle bosses are defeated,
returning to the Survivors' Camp (POI 1200) triggers a story beat
`chaos_starstone_revealed` — Marwen tells the player to bring the four
boss weapons to the camp. Completing it gates the climactic boss
`chaos_starstone_avatar` ("Echo of the First Cut"), which triggers at
the same POI when the player has frost_axe + fire_blade + dragonslayer +
stormbringer in inventory. Reward: 2000 gold, the Starstone Shard
accessory (ATK+3 / DEF+3 / HP+20), and the Starstone Awakened boon.

Sprint's "first 1 km of session" anchors on
`session_start_distance_m`, which resets when the player resumes
walking after a >60 s idle gap. So between play sessions the boost
re-applies; you can't farm by just standing still and walking again.

Retroactive grants: `POST /admin/grant_boon_choice {player_id,
event_id?}` queues a picker for a player who beat a climactic event
before `grants_boon: true` was added (e.g. the original Frost Lord
victory before this system landed). Same deterministic 3-of-N as the
natural grant.

### Leveling
- Walking distance = XP. Curve is **geometric**, each level-up gap 10 %
  larger than the last. Cumulative meters to reach level N:
  `10000 · (1.1^(N−1) − 1)`. Source of truth: `questlib::leveling`.
- Lvl 2: 1.0 km · Lvl 5: 4.6 km · Lvl 10: 13.6 km · Lvl 20: 51 km ·
  Lvl 28: 121 km · Lvl 30: 164 km
- (An older `3·N³ + 70·N` cubic formula was the original design — the
  doc here used to quote those numbers but the code switched at some
  point; the current geometric curve is what's running.)

### Ambient effects
- **Clouds** drift across the overworld in `crates/gameclient/src/ambient.rs`.
  Each of 3 variant textures (192×96) is shaped by 4-octave fBm value
  noise × an elliptical radial falloff — gives the irregular-edge look
  of real clouds rather than a stamped blob. 18 instances are spawned
  cycling through the textures so the sky isn't made of 18 clones, each
  with randomized position, scale (2.5–4.5), alpha (0.15–0.30 — low so
  overlapping clouds build density naturally), and west→east drift speed
  (8–14 px/s). Clouds wrap horizontally: off the right edge → respawn
  left with a new vertical position. Z=20 puts them above terrain/player
  but below UI. Hidden automatically while inside an interior
  (`MyPlayerState.location.is_some()`). Randomness uses
  `js_sys::Math::random()` — no `rand` dep on the WASM side.
- **Cloud shadows.** Each cloud has a black-tinted `CloudShadow` child
  rendered at z=0.5 (above ground, below sprites). Per frame,
  `update_cloud_shadows` reprojects it using the current sun/moon
  position from `DayNightCycle`: `offset = -sun_dir.xy * CLOUD_HEIGHT /
  sun_dir.z`, clamped to 80 px so horizon-grazing suns don't fling the
  shadow across the map. Alpha is `base_alpha × (1 − night_alpha)` so
  shadows fade completely by midnight — moonlight is too weak to cast
  a crisp one. F8 debug sun overrides too.
- **Rain.** ~30 % of clouds get a `RainyCloud` marker on spawn — rendered
  darker (muted grey-blue tint) and emit rain drops (`DROPS_PER_CLOUD_PER_SEC`)
  from the cloud's current position. Drops are 1×5 px blue-white sprites
  falling straight down at 240 px/s, despawning after they've travelled
  `DROP_FALL_DISTANCE` below the spawn line. Rain drifts with its cloud
  naturally — no global "it's raining" toggle, each storm is local.
  Drops despawn entirely on interior entry; rainy clouds pause emission
  since they're `Visibility::Hidden` under the same rule as all clouds.
- **World lighting overlay** (F6 dev toggle). `crates/gameclient/src/terrain/lighting.rs`
  bakes a 1600×1280 RGBA darkness overlay over the whole world when
  enabled. Heightmap from biomes (water=0, grass=0.4, mountain=1.0) →
  three 3×3 box blurs → Lambertian slope-lighting against a fixed
  sun vector (-0.65, -0.65, 0.8) matching the cloud-shadow direction.
  Output is quantized to 5 brightness bands so it reads stylized, not
  smoothly blurred, against the pixel art. Plus a per-pixel shoreline
  bevel: land pixels within SHORELINE_BEVEL_WIDTH_PX of a water-tile
  rectangle get a cosine-falloff darkening (sun-independent — beaches
  feel beveled from every angle). Toggle off despawns the sprite.
- **Parked for later (v2 ground look):** a fully procedural shader
  ground where tiles are just a biome-map lookup and a WGSL
  `Material2d` renders curved roads / curved shorelines / animated
  water / fBm-detailed biomes. Current Phase 1 overlay is the cheap
  prototype to see if slope-based lighting fits the pixel-art style
  before we commit to the shader effort.

### Sound effects
- 8-bit square-wave blips synthesized on-the-fly in `crates/gameclient/src/sfx.rs`.
  No audio assets shipped — each sound is a short note sequence built with
  the browser's Web Audio API (`AudioContext` + `OscillatorNode` + `GainNode`).
- Three events, all detected client-side from state deltas:
  - **RouteArrived** (planned route went empty): E5 → C5 soft descending
  - **LevelUp** (character level increased): C5 → E5 → G5 triad
  - **CombatVictory** (combat went active → inactive): G4 → C5 → E5 → G5 fanfare
- Gold gain (chest, monster loot, quest reward) is intentionally
  silent — too frequent to sound good. CombatVictory covers the
  "defeated an enemy" feedback; chest opens show floating `+N gold`
  text and a notification banner.
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
- `POST /forge_upgrade` — `{"player_id":"...","item_id":"iron_sword"}` — spend
  gold to add +1 to an equipped item's stat. Cost = 500 × (current_level + 1).
  Max level 5 per item; per-slot effects: Weapon +1 ATK, Armor +1 DEF,
  Accessory +2 Max HP, Feet +1 % speed, Toe Rings +1 ATK. Server enforces
  equip / level-cap / gold checks. Forgemaster Kael is at Coastal Town (POI 1).
- `GET /shops?player_id=X` — shops the player has discovered
  (`revealed_shops` list). Populated two ways: (1) first time the player
  completes a shop event, and (2) when an NPC dialogue grants
  `EventOutcome::RevealShop { shop_event_id }`. Used by the client to
  draw "Shop: Name" labels on TAB.
- `GET /version` — returns `{"version": N}` parsed from index.html's `?v=N`
  cache-bust number. Clients poll this to detect stale WASM after a deploy
  and surface a Refresh banner. Cached on first hit per process.
- `GET /daynight` — `{"time_s": X, "cycle_seconds": Y}` so every client
  sees the same time-of-day. Stateless: `time_s = unix_now %
  cycle_seconds`, so restarts / deploys don't jump the cycle. Client
  fetches on enter-game and every 60 s thereafter; between polls it
  keeps advancing `time_s` locally from `Time::delta_secs()`.
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
# Pass player_id to route to that player's adventure bundle. Without
# it, the default catalog (frost_quest) is targeted, which is wrong
# for chaos events.
curl -s -X POST $BASE/admin/reset_event \
  -H 'Content-Type: application/json' -H "X-Admin-Token: $TOKEN" \
  -d '{"event_id":"chaos_frost_queen","status":"completed","player_id":"<uuid>"}'
curl -s -X POST $BASE/admin/revoke_completion \
  -H 'Content-Type: application/json' -H "X-Admin-Token: $TOKEN" \
  -d '{"player_id":"<uuid>","event_id":"find_traveler"}'

# Diagnose / unstick combat. dump_combat returns the in-memory combat
# map; clear_combat drops one key (or all if event_id is omitted). Use
# when a player sits on a mobile monster and combat won't start —
# check the deploy logs for `[mobile_entity] contact-skip:` warnings.
curl -s -X POST $BASE/admin/dump_combat \
  -H 'Content-Type: application/json' -H "X-Admin-Token: $TOKEN" \
  -d '{}'
curl -s -X POST $BASE/admin/clear_combat \
  -H 'Content-Type: application/json' -H "X-Admin-Token: $TOKEN" \
  -d '{"event_id":"mobile_monster:grassland_wolf_1"}'

# Retroactive boon grant — queue a 3-of-N picker for a player who
# beat a climactic event before grants_boon was added to its kind.
# event_id is optional (defaults to "manual_grant"); same id keeps
# the offered set stable across re-grants without an interim select.
curl -s -X POST $BASE/admin/grant_boon_choice \
  -H 'Content-Type: application/json' -H "X-Admin-Token: $TOKEN" \
  -d '{"player_id":"<uuid>","event_id":"tower_20"}'
```

## State Persistence

- `dev_state.json` — auto-saved every 30s by Game Master, and on SIGTERM
- Contains: player positions, gold, inventory, fog, per-event status
- Versioned (`SaveData.version`); `migrate_save` handles older versions
- Item ids that disappear from the catalog are pruned from inventory/equipment on load
- Event definitions are re-read fresh from `EVENTS_PATH` every startup — only per-event status is carried over, so content updates (shops, triggers, outcomes) don't require wiping the save
- Delete to reset locally: `rm dev_state.json`

## TODO

- LLM adventure skill (`/adventure` generates events from POI JSON)
- **Mobile entities Phase 2**: reactive behaviors (chase / flee), full
  dialogue UI activation from NPC contact (currently a notification
  only), follow-path behavior, day/night schedules. Phase 1 (Wander +
  Patrol + combat-on-contact) shipped — see notes below and the spec
  at `adventures/MOBILE_ENTITIES.md`.
- Real auth (currently /notifications only does a sanity check; anyone with a
  known player_id can poll that player's queue)
- Delete or revive the excluded `walker` crate (currently dead-on-disk)
- Shared-goal widgets / team stats in HUD to reinforce co-op
