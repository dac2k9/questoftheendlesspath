#!/usr/bin/env ruby
# Grant a boon choice on the deployed server. One-shot tool for the
# retroactive case where a player beat a climactic event before
# `grants_boon: true` was added to its kind. Future boss victories
# trigger the picker automatically — no need to run this for those.
#
# Usage:
#   1. Edit ADMIN_TOKEN below (or `export ADMIN_TOKEN=...`).
#   2. Run:
#        ruby tools/grant_boon.rb
#      Or filter to a specific player by name substring:
#        ruby tools/grant_boon.rb daniel
#
# Output is the offered 3 boon ids — reload the browser and the
# picker modal should appear.

require 'net/http'
require 'json'
require 'uri'

# Edit this if you don't want to use the env var.
ADMIN_TOKEN = ENV['ADMIN_TOKEN'] || 'PASTE_YOUR_TOKEN_HERE'

# Override via env if needed (e.g. local dev: BASE=http://localhost:3001).
BASE     = ENV['BASE']     || 'https://questoftheendlesspath-latest.onrender.com'
EVENT_ID = ENV['EVENT_ID'] || 'tower_20'

if ADMIN_TOKEN.start_with?('PASTE_')
  abort "ERROR: set ADMIN_TOKEN at the top of the script or in your shell env."
end

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

def post_admin(path, body)
  uri = URI("#{BASE}#{path}")
  req = Net::HTTP::Post.new(uri.request_uri)
  req['Content-Type'] = 'application/json'
  req['X-Admin-Token'] = ADMIN_TOKEN
  req.body = body.to_json
  res = http_for(uri).request(req)
  [res.code, res.body]
end

# 1. Fetch /players (hash keyed by player_id).
players = get_json('/players')
abort "no players on the server" if players.empty?

# 2. Pick the player. CLI arg = case-insensitive name substring filter;
# absent → the first (or only) player on the server.
filter = ARGV[0]
pick = if filter && !filter.empty?
  players.find { |_id, p| (p['name'] || '').downcase.include?(filter.downcase) }
else
  players.first
end
abort "no player matches '#{filter}' — names: #{players.values.map { |p| p['name'] }.inspect}" unless pick

pid, info = pick
puts "Player: #{info['name']} (#{pid})"
puts "Already-owned boons: #{(info['boons'] || []).inspect}"

# 3. Grant.
code, body = post_admin('/admin/grant_boon_choice', { player_id: pid, event_id: EVENT_ID })
puts ""
puts "HTTP #{code}: #{body}"

unless code == '200'
  abort "Grant failed. Common causes: ADMIN_TOKEN wrong, server not yet redeployed " \
        "with the boons feature, or player already owns every boon."
end

resp = JSON.parse(body) rescue {}
choices = resp['choices'] || []
puts ""
puts "Offered: #{choices.join(', ')}"
puts "Reload your browser (Cmd-Shift-R) and the picker should appear."
