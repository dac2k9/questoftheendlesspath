#!/usr/bin/env ruby
# Boon-admin helper. Three modes:
#
#   ruby tools/grant_boon.rb                  # list all players + boons
#   ruby tools/grant_boon.rb <name>           # grant a 3-of-N picker
#   ruby tools/grant_boon.rb cancel <name>    # clear a pending picker
#
# `<name>` is a case-insensitive substring of the player's display
# name. Match must be unique — if multiple players match, the script
# aborts with the candidate list. Default mode (no args) lists every
# player so you can see who's on the server before granting.
#
# Edit ADMIN_TOKEN below or set ADMIN_TOKEN in your shell env.

require 'net/http'
require 'json'
require 'uri'

ADMIN_TOKEN = ENV['ADMIN_TOKEN'] || 'PASTE_YOUR_TOKEN_HERE'
BASE        = ENV['BASE']        || 'https://questoftheendlesspath-latest.onrender.com'
EVENT_ID    = ENV['EVENT_ID']    || 'tower_20'

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
  abort "GET #{path}: HTTP #{res.code}\n  body: #{res.body[0,300]}" unless res.is_a?(Net::HTTPSuccess)
  JSON.parse(res.body)
end

def post_admin(path, body)
  uri = URI("#{BASE}#{path}")
  req = Net::HTTP::Post.new(uri.request_uri)
  req['Content-Type']  = 'application/json'
  req['X-Admin-Token'] = ADMIN_TOKEN
  req.body = body.to_json
  [http_for(uri).request(req).then { |r| [r.code, r.body] }].flatten
end

def parse_args
  args = ARGV.dup
  cmd = args.shift
  case cmd
  when nil, ''
    [:list, nil]
  when 'cancel'
    name = args.shift
    abort "usage: ruby tools/grant_boon.rb cancel <name>" if name.nil? || name.empty?
    [:cancel, name]
  else
    [:grant, cmd]
  end
end

def find_unique(players, name_substr)
  needle = name_substr.downcase
  matches = players.select { |p| (p['name'] || '').downcase.include?(needle) }
  case matches.size
  when 0
    abort "no player matches '#{name_substr}'\nnames on server: #{players.map { |p| p['name'] }.inspect}"
  when 1
    matches.first
  else
    abort "'#{name_substr}' matches multiple — narrow it down: #{matches.map { |p| p['name'] }.inspect}"
  end
end

def list_players(players)
  puts "#{players.size} players on #{BASE}:"
  players.sort_by { |p| (p['name'] || '').downcase }.each do |p|
    pending = p['pending_boon_choice'] ? "  PENDING: #{p['pending_boon_choice']['choices'].inspect}" : ''
    boons = p['boons'] || []
    puts "  #{p['name'].to_s.ljust(20)}  id=#{p['id']}  boons=#{boons.inspect}#{pending}"
  end
  puts
  puts "Grant: ruby tools/grant_boon.rb <name-substring>"
  puts "Cancel: ruby tools/grant_boon.rb cancel <name-substring>"
end

mode, name = parse_args
players = get_json('/players')
abort "no players on the server" if players.nil? || players.empty?

case mode
when :list
  list_players(players)
when :grant
  info = find_unique(players, name)
  pid = info['id']
  if info['pending_boon_choice']
    puts "Note: #{info['name']} already has a pending picker; granting will overwrite it."
  end
  code, body = post_admin('/admin/grant_boon_choice', { player_id: pid, event_id: EVENT_ID })
  puts "Granted to #{info['name']} (#{pid})"
  puts "HTTP #{code}: #{body}"
when :cancel
  info = find_unique(players, name)
  pid = info['id']
  unless info['pending_boon_choice']
    puts "No pending picker on #{info['name']} — nothing to cancel."
    exit 0
  end
  code, body = post_admin('/admin/clear_boon_choice', { player_id: pid })
  puts "Cleared pending picker for #{info['name']} (#{pid})"
  puts "HTTP #{code}: #{body}"
end
