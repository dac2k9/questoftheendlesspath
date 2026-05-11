#!/usr/bin/env ruby
# Chaos adventure smoketest. Drives a player through the chaos
# quest chain end-to-end and reports pass/fail per step. Useful for
# catching content regressions without a manual playtest.
#
# What it does:
#   1. Switch the target player to the chaos adventure (resets level
#      / gold / inventory, keeps boons + identity).
#   2. Teleport to each quest POI in order (intro camp → 4 gates →
#      4 castle approaches → 4 castles), letting one server tick
#      elapse between each.
#   3. Complete browser-requiring events via /events/<id>/complete.
#   4. Verify items appear in inventory at the expected steps.
#   5. Report a pass/fail checklist.
#
# Limitations:
#   - Doesn't actually fight bosses (combat needs walking speed to
#     fill the charge bar). Just checks the boss event reaches Active.
#   - The 2 km distance gate on Hael's reward is bypassed by directly
#     bumping total_distance_m via a /debug_walk loop (slow) — skip
#     that step with SKIP_DISTANCE=1 if you just want to check the
#     other quest chain.
#
# Usage:
#   ADMIN_TOKEN=... ruby tools/chaos_smoketest.rb [player_name]
#
# Defaults: BASE=http://localhost:3001, picks the first player on
# the server if no name is given.

require 'net/http'
require 'json'
require 'uri'

ADMIN_TOKEN = ENV['ADMIN_TOKEN'] || abort("set ADMIN_TOKEN")
BASE        = ENV['BASE']        || 'http://localhost:3001'
SKIP_DISTANCE = ENV['SKIP_DISTANCE'] == '1'

def http_for(uri)
  http = Net::HTTP.new(uri.host, uri.port)
  http.use_ssl = uri.scheme == 'https'
  http
end

def get_json(path)
  uri = URI("#{BASE}#{path}")
  res = http_for(uri).request(Net::HTTP::Get.new(uri.request_uri))
  unless res.is_a?(Net::HTTPSuccess)
    abort "GET #{path}: HTTP #{res.code}\n  body: #{res.body[0,300]}"
  end
  JSON.parse(res.body)
end

def post_json(path, body, admin: false)
  uri = URI("#{BASE}#{path}")
  req = Net::HTTP::Post.new(uri.request_uri)
  req['Content-Type']  = 'application/json'
  req['X-Admin-Token'] = ADMIN_TOKEN if admin
  req.body = body.to_json
  res = http_for(uri).request(req)
  [res.code, res.body]
end

# Try to find a player by case-insensitive name substring, or pick
# the first player on the server.
def find_player(filter)
  players = get_json('/players')
  abort "no players on the server" if players.nil? || players.empty?
  if filter && !filter.empty?
    f = filter.downcase
    matches = players.select { |p| (p['name'] || '').downcase.include?(f) }
    abort "no player matches '#{filter}' — names: #{players.map { |p| p['name'] }.inspect}" if matches.empty?
    abort "'#{filter}' matches multiple — narrow it down: #{matches.map { |p| p['name'] }.inspect}" if matches.size > 1
    matches.first
  else
    players.first
  end
end

# Snapshot of the target player's chaos-relevant state.
def state_for(pid)
  p = get_json('/players').find { |q| q['id'] == pid }
  abort "player #{pid} disappeared" unless p
  {
    adventure_id: p['adventure_id'],
    tile:         [p['map_tile_x'], p['map_tile_y']],
    items:        (p['inventory'] || []).map { |s| s['item_id'] },
    completed:    p['completed_events'] || [],
    gates:        p['unlocked_travel_gates'] || [],
    gold:         p['gold'],
    distance:     p['total_distance_m'],
  }
end

# Print a checklist line.
def check(label, ok, detail = '')
  prefix = ok ? "\e[32m  OK\e[0m" : "\e[31mFAIL\e[0m"
  detail = " — #{detail}" unless detail.empty?
  puts "  [#{prefix}] #{label}#{detail}"
  $passes += 1 if ok
  $fails  += 1 unless ok
end

$passes = 0
$fails  = 0

# ── Run ───────────────────────────────────────

player = find_player(ARGV[0])
pid    = player['id']
puts "Player: #{player['name']} (#{pid})"
puts "Base:   #{BASE}"
puts

# Step 1: switch to chaos.
puts "Step 1: Switch to chaos adventure"
code, body = post_json('/start_new_adventure', { player_id: pid, adventure_id: 'chaos' })
check("HTTP 200 from /start_new_adventure", code == '200', body[0,100])
s = state_for(pid)
check("adventure_id == 'chaos'", s[:adventure_id] == 'chaos', s[:adventure_id])
check("inventory wiped", s[:items].empty?, s[:items].inspect)
check("completed_events wiped", s[:completed].empty?, s[:completed].inspect)
puts

# Step 2: Survivors' Camp (50, 40).
puts "Step 2: Survivors' Camp — intro dialogue"
post_json('/admin/teleport', { player_id: pid, x: 50, y: 40 }, admin: true)
sleep 2 # let the tick fire the event
post_json('/events/chaos_intro/complete', { player_id: pid })
sleep 1
s = state_for(pid)
check("chaos_intro completed", s[:completed].include?('chaos_intro'))
check("got health_potion", s[:items].include?('health_potion'))
check("got 50 gold", s[:gold] >= 50, "gold=#{s[:gold]}")
puts

# Step 3: East Gate scout (70, 28) — Flame quest hub.
puts "Step 3: East Gate — Flame key handoff"
post_json('/admin/teleport', { player_id: pid, x: 70, y: 28 }, admin: true)
sleep 2
post_json('/events/chaos_east_gate_scout/complete', { player_id: pid })
sleep 1
s = state_for(pid)
check("ember_talisman in inventory", s[:items].include?('ember_talisman'))
check("East Gate unlocked", s[:gates].include?(1101))
puts

# Step 4: South Gate hermit (30, 58) — Shadow quest hub.
puts "Step 4: South Gate — Shadow key handoff"
post_json('/admin/teleport', { player_id: pid, x: 30, y: 58 }, admin: true)
sleep 2
post_json('/events/chaos_south_gate_hermit/complete', { player_id: pid })
sleep 1
s = state_for(pid)
check("voidlight_lantern in inventory", s[:items].include?('voidlight_lantern'))
check("South Gate unlocked", s[:gates].include?(1102))
puts

# Step 5: West Gate wanderer (72, 58) — Storm quest hub.
puts "Step 5: West Gate — Storm key handoff"
post_json('/admin/teleport', { player_id: pid, x: 72, y: 58 }, admin: true)
sleep 2
post_json('/events/chaos_west_gate_wanderer/complete', { player_id: pid })
sleep 1
s = state_for(pid)
check("grounding_charm in inventory", s[:items].include?('grounding_charm'))
check("West Gate unlocked", s[:gates].include?(1103))
puts

# Step 6: Hael's Spire — frost quest dialogue (35, 25).
puts "Step 6: Spire of Hael — frost quest dialogue"
post_json('/admin/teleport', { player_id: pid, x: 35, y: 25 }, admin: true)
sleep 2
post_json('/events/chaos_hael_quest/complete', { player_id: pid })
sleep 1
s = state_for(pid)
check("chaos_hael_quest completed", s[:completed].include?('chaos_hael_quest'))
puts

# Step 7: walk 2 km to unlock the frost reward.
if SKIP_DISTANCE
  puts "Step 7: SKIP_DISTANCE=1 — skipping the 2 km walk gate"
  puts
else
  puts "Step 7: Walking 2 km in chaos to trigger chaos_hael_reward"
  post_json('/debug_walk', { player_id: pid, speed: 100.0 }) # synthetic high speed
  sleep 1
  # Tick the player along — bump distance manually each second until
  # we hit 2 km. The tick adds delta from current_speed_kmh / 3.6.
  loop do
    sleep 1
    s = state_for(pid)
    break if s[:distance] >= 2050
    print "  walked #{s[:distance].round}m \r"
  end
  puts "  walked 2000m+ — good"
  post_json('/debug_walk', { player_id: pid, speed: 0 })
  # Re-teleport to Hael to refire the trigger.
  post_json('/admin/teleport', { player_id: pid, x: 35, y: 25 }, admin: true)
  sleep 2
  post_json('/events/chaos_hael_reward/complete', { player_id: pid })
  sleep 1
  s = state_for(pid)
  check("frostbound_key in inventory", s[:items].include?('frostbound_key'))
  puts
end

# Step 8: castle reachability checks. We just verify the boss event
# becomes Active when standing on the castle tile with the right key
# in inventory. We don't run the combat itself (charge bar needs
# treadmill speed).
boss_pairs = [
  ["chaos_frost_queen",       [14, 12], "frostbound_key",    SKIP_DISTANCE ? nil : "frostbound_key"],
  ["chaos_lord_flame",        [85, 18], "ember_talisman",    "ember_talisman"],
  ["chaos_hierophant_shadow", [18, 68], "voidlight_lantern", "voidlight_lantern"],
  ["chaos_stormbinder",       [88, 68], "grounding_charm",   "grounding_charm"],
]
boss_pairs.each do |event_id, (x, y), required_item, expected_have|
  label = event_id.sub('chaos_', '').tr('_', ' ').capitalize
  puts "Step #{label}: castle at (#{x}, #{y})"
  post_json('/admin/teleport', { player_id: pid, x: x, y: y }, admin: true)
  sleep 2
  s = state_for(pid)
  if expected_have
    check("has #{required_item}", s[:items].include?(required_item))
  end
  # Server-side `events/active` filter shows whether the boss event
  # would fire for this player. We don't auto-complete it.
  active = get_json("/events/active?player_id=#{pid}")
  has_boss = active.any? { |e| e['id'] == event_id }
  check("#{event_id} reaches Active", has_boss, active.map { |e| e['id'] }.inspect)
  puts
end

puts "──────────────────────────────────────"
puts "Result: \e[32m#{$passes} passed\e[0m, \e[31m#{$fails} failed\e[0m"
exit($fails > 0 ? 1 : 0)
