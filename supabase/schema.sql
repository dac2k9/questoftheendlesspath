-- Quest of the Endless Path — Supabase Schema
-- Deploy via Supabase SQL Editor or CLI

-- Games
create table if not exists games (
  id uuid primary key default gen_random_uuid(),
  join_code text unique not null,
  adventure_name text not null,
  status text default 'active' check (status in ('active','paused','completed')),
  created_at timestamptz default now()
);

-- Players
create table if not exists players (
  id uuid primary key default gen_random_uuid(),
  game_id uuid references games(id),
  name text not null,
  avatar text default 'knight',
  -- Live treadmill data (written by walker)
  current_speed_kmh real default 0,
  total_distance_m integer default 0,
  current_incline real default 0,
  -- Game state (written by game master)
  map_position_km real default 0,
  gold integer default 0,
  is_walking boolean default false,
  is_browser_open boolean default false,
  is_blocked boolean default false,
  blocked_at_km real,
  inventory jsonb default '[]'::jsonb,
  -- Fog of war: base64-encoded bitfield (100x80 = 8000 bits = 1000 bytes)
  -- Each bit = 1 tile revealed. Written by game master.
  revealed_tiles text default '',
  -- Current tile position on the map grid (written by game master)
  map_tile_x integer default 0,
  map_tile_y integer default 0,
  last_seen_at timestamptz default now()
);

-- Events (populated by game master from adventure.yaml)
create table if not exists events (
  id uuid primary key default gen_random_uuid(),
  game_id uuid references games(id),
  at_km real not null,
  event_type text not null,
  name text not null,
  data jsonb not null,
  requires_all_players boolean default false,
  requires_browser boolean default false,
  status text default 'pending' check (status in ('pending','active','completed','skipped')),
  triggered_at timestamptz,
  completed_at timestamptz
);

-- Boss encounters
create table if not exists boss_encounters (
  id uuid primary key default gen_random_uuid(),
  game_id uuid references games(id),
  event_id uuid references events(id),
  boss_name text not null,
  max_hp integer not null,
  current_hp integer not null,
  defeated boolean default false,
  started_at timestamptz default now()
);

-- Game log (history for stats)
create table if not exists game_log (
  id bigint generated always as identity primary key,
  game_id uuid references games(id),
  player_id uuid references players(id),
  event_type text not null,
  data jsonb default '{}'::jsonb,
  created_at timestamptz default now()
);

-- Atomic boss damage function
create or replace function damage_boss(p_boss_id uuid, p_dmg integer)
returns integer as $$
  update boss_encounters
  set current_hp = greatest(0, current_hp - p_dmg),
      defeated = (greatest(0, current_hp - p_dmg) = 0)
  where id = p_boss_id
  returning current_hp;
$$ language sql;

-- Browser heartbeat (callable by anon key via RPC)
create or replace function browser_heartbeat(p_player_id uuid)
returns void as $$
  update players set is_browser_open = true, last_seen_at = now()
  where id = p_player_id;
$$ language sql security definer;

-- Enable RLS on all tables
alter table games enable row level security;
alter table players enable row level security;
alter table events enable row level security;
alter table boss_encounters enable row level security;
alter table game_log enable row level security;

-- Anon key = read only
create policy "games_read" on games for select using (true);
create policy "players_read" on players for select using (true);
create policy "events_read" on events for select using (true);
create policy "boss_read" on boss_encounters for select using (true);
create policy "log_read" on game_log for select using (true);

-- Enable Realtime for tables the browser subscribes to
alter publication supabase_realtime add table players;
alter publication supabase_realtime add table events;
alter publication supabase_realtime add table boss_encounters;
alter publication supabase_realtime add table games;

-- Indexes for common queries
create index if not exists idx_players_game_id on players(game_id);
create index if not exists idx_events_game_id on events(game_id);
create index if not exists idx_events_status on events(game_id, status);
create index if not exists idx_boss_game_id on boss_encounters(game_id, defeated);
create index if not exists idx_game_log_game_id on game_log(game_id);
