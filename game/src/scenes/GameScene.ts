import Phaser from "phaser";
import { supabase, GAME_WIDTH, GAME_HEIGHT } from "../config";

const FONT = '"Press Start 2P", monospace';
const TILE = 16; // tile size in pixels
const MAP_W = 200; // map width in tiles
const MAP_H = 150; // map height in tiles
const TOTAL_KM = 75;
const CAMERA_ZOOM = 3;

// Terrain types
const T = {
  WATER: 0,
  GRASS: 1,
  FOREST: 2,
  MOUNTAIN: 3,
  SAND: 4,
  SWAMP: 5,
  LAVA: 6,
  PATH: 7,
  TOWN: 8,
  CASTLE: 9,
  SNOW: 10,
};

// Terrain colors
const COLORS: Record<number, number> = {
  [T.WATER]: 0x3366aa,
  [T.GRASS]: 0x55aa55,
  [T.FOREST]: 0x2d6b2d,
  [T.MOUNTAIN]: 0x887766,
  [T.SAND]: 0xccbb88,
  [T.SWAMP]: 0x445533,
  [T.LAVA]: 0x993311,
  [T.PATH]: 0xaa9966,
  [T.TOWN]: 0xbb8855,
  [T.CASTLE]: 0x999999,
  [T.SNOW]: 0xddddee,
};

interface PlayerData {
  id: string;
  name: string;
  avatar: string;
  current_speed_kmh: number;
  map_position_km: number;
  gold: number;
  is_walking: boolean;
  is_browser_open: boolean;
  is_blocked: boolean;
}

interface EventData {
  id: string;
  at_km: number;
  event_type: string;
  name: string;
  data: Record<string, unknown>;
  status: string;
}

// Path waypoints — the route winds through the world map
// Each point is [tileX, tileY, km_at_this_point]
const PATH_WAYPOINTS: [number, number, number][] = [
  // Meadowlands — gentle rolling start
  [20, 120, 0],
  [30, 115, 1],
  [40, 110, 2],
  [50, 105, 3],
  [55, 100, 4],
  [60, 95, 5],
  [65, 88, 6],
  [70, 82, 7],
  [75, 78, 8],
  [78, 72, 9],
  // Whispering Woods — winding through forest
  [82, 68, 10],
  [88, 65, 11],
  [95, 62, 12],
  [100, 58, 13],
  [105, 55, 14],
  [108, 50, 15],
  [112, 46, 16],
  [115, 42, 17],
  [118, 38, 18],
  // Highlands — climbing up
  [122, 35, 19],
  [128, 32, 21],
  [134, 30, 23],
  [138, 28, 25],
  [142, 30, 27],
  [145, 33, 28],
  // Stormcrag Pass — narrow mountain pass
  [148, 38, 29],
  [150, 44, 31],
  [152, 50, 33],
  [150, 56, 35],
  [147, 60, 37],
  // Darkmarsh — south through swamp
  [143, 65, 38],
  [140, 70, 40],
  [136, 75, 42],
  [132, 80, 44],
  [128, 84, 46],
  [125, 88, 47],
  // Kingdom of Ember — east through ruins
  [120, 90, 48],
  [115, 92, 50],
  [110, 95, 52],
  [108, 100, 54],
  [110, 105, 56],
  // Ashlands — volcanic terrain
  [115, 108, 57],
  [120, 110, 59],
  [128, 108, 61],
  [135, 105, 63],
  [140, 100, 65],
  [145, 95, 66],
  // Dragon's Peak — final ascent
  [150, 88, 67],
  [155, 82, 69],
  [160, 76, 71],
  [165, 70, 73],
  [168, 65, 74],
  [170, 60, 75],
];

export class GameScene extends Phaser.Scene {
  private players: Map<string, PlayerData> = new Map();
  private playerSprites: Map<string, Phaser.GameObjects.Container> = new Map();
  private gameEvents: EventData[] = [];
  private gameId = "";
  private playerId = "";
  private mapTiles!: Phaser.GameObjects.Graphics;
  private minimapGfx!: Phaser.GameObjects.Graphics;

  constructor() {
    super({ key: "GameScene" });
  }

  create(): void {
    this.gameId = this.registry.get("gameId") as string;
    this.playerId = this.registry.get("playerId") as string;

    // World bounds + zoom
    this.cameras.main.setBounds(0, 0, MAP_W * TILE, MAP_H * TILE);
    this.cameras.main.setZoom(CAMERA_ZOOM);

    // Generate and render the world map
    const terrain = this.generateTerrain();
    this.renderTerrain(terrain);
    this.renderPath();
    this.renderEventMarkers();

    // Minimap
    this.createMinimap(terrain);

    // Start heartbeat + subscriptions
    this.startHeartbeat();
    this.subscribeToUpdates();
    this.loadInitialState();
  }

  update(_time: number, _delta: number): void {
    for (const [id, data] of this.players) {
      const sprite = this.playerSprites.get(id);
      if (!sprite) continue;

      const [tx, ty] = this.kmToPixel(data.map_position_km);
      sprite.x += (tx - sprite.x) * 0.08;
      sprite.y += (ty - sprite.y) * 0.08;

      // Update walk indicator
      const indicator = sprite.getData("walkDot") as Phaser.GameObjects.Arc;
      if (indicator) indicator.setVisible(data.is_walking);
    }

    // Camera follows our player
    const mySprite = this.playerSprites.get(this.playerId);
    if (mySprite) {
      const cam = this.cameras.main;
      cam.scrollX += (mySprite.x - GAME_WIDTH / 2 - cam.scrollX) * 0.05;
      cam.scrollY += (mySprite.y - GAME_HEIGHT / 2 - cam.scrollY) * 0.05;
    }

    // Update minimap player dots
    this.updateMinimap();
  }

  // ── Terrain Generation ──────────────────────────────

  private generateTerrain(): number[][] {
    const map: number[][] = [];
    for (let y = 0; y < MAP_H; y++) {
      map[y] = [];
      for (let x = 0; x < MAP_W; x++) {
        map[y][x] = this.terrainAt(x, y);
      }
    }

    // Carve path (wide, 3 tiles)
    for (let i = 0; i < PATH_WAYPOINTS.length - 1; i++) {
      const [x1, y1] = PATH_WAYPOINTS[i];
      const [x2, y2] = PATH_WAYPOINTS[i + 1];
      this.carvePath(map, x1, y1, x2, y2);
    }

    return map;
  }

  private terrainAt(x: number, y: number): number {
    // Edges are water
    if (x < 5 || x > MAP_W - 6 || y < 5 || y > MAP_H - 6) return T.WATER;
    // Shore transition
    if (x < 10 || x > MAP_W - 11 || y < 10 || y > MAP_H - 11) {
      return this.smoothNoise(x, y, 4) > 0.5 ? T.WATER : T.GRASS;
    }

    const zone = this.getZoneAt(x, y);
    // Use smooth noise at different scales for natural-looking clusters
    const n1 = this.smoothNoise(x, y, 8);   // large features
    const n2 = this.smoothNoise(x, y, 4);   // medium features
    const n3 = this.smoothNoise(x + 50, y + 50, 6); // offset for variety

    switch (zone) {
      case 0: // Meadowlands — mostly grass with forest clusters
        if (n1 > 0.7) return T.FOREST;
        if (n2 > 0.85) return T.WATER;
        return T.GRASS;
      case 1: // Whispering Woods — dense forest with clearings
        if (n1 < 0.25) return T.GRASS;
        if (n2 > 0.8) return T.WATER;
        return T.FOREST;
      case 2: // Highlands — grass, mountains, some forest
        if (n1 > 0.65) return T.MOUNTAIN;
        if (n2 > 0.7) return T.FOREST;
        if (n3 > 0.85) return T.SNOW;
        return T.GRASS;
      case 3: // Stormcrag Pass — mountains and snow
        if (n1 > 0.4) return T.MOUNTAIN;
        if (n2 > 0.6) return T.SNOW;
        return T.MOUNTAIN;
      case 4: // Darkmarsh — swamp with water pools
        if (n1 > 0.65) return T.WATER;
        if (n2 > 0.5) return T.SWAMP;
        return T.SWAMP;
      case 5: // Kingdom of Ember — sand and ruins
        if (n1 > 0.7) return T.MOUNTAIN;
        if (n2 < 0.3) return T.GRASS;
        return T.SAND;
      case 6: // Ashlands — volcanic
        if (n1 > 0.6) return T.LAVA;
        if (n2 > 0.5) return T.MOUNTAIN;
        return T.SAND;
      case 7: // Dragon's Peak — lava and rock
        if (n1 > 0.5) return T.LAVA;
        return T.MOUNTAIN;
      default:
        return T.GRASS;
    }
  }

  // Simple value noise for smooth terrain clusters
  private smoothNoise(x: number, y: number, scale: number): number {
    const sx = x / scale;
    const sy = y / scale;
    const ix = Math.floor(sx);
    const iy = Math.floor(sy);
    const fx = sx - ix;
    const fy = sy - iy;

    const a = (this.hash(ix, iy) % 1000) / 1000;
    const b = (this.hash(ix + 1, iy) % 1000) / 1000;
    const c = (this.hash(ix, iy + 1) % 1000) / 1000;
    const d = (this.hash(ix + 1, iy + 1) % 1000) / 1000;

    // Bilinear interpolation
    const top = a + (b - a) * fx;
    const bot = c + (d - c) * fx;
    return top + (bot - top) * fy;
  }

  private getZoneAt(x: number, y: number): number {
    // Determine zone based on proximity to path waypoints
    let closestKm = 0;
    let closestDist = Infinity;
    for (const [wx, wy, km] of PATH_WAYPOINTS) {
      const d = Math.hypot(x - wx, y - wy);
      if (d < closestDist) {
        closestDist = d;
        closestKm = km;
      }
    }
    if (closestKm < 9) return 0;
    if (closestKm < 18) return 1;
    if (closestKm < 28) return 2;
    if (closestKm < 37) return 3;
    if (closestKm < 47) return 4;
    if (closestKm < 56) return 5;
    if (closestKm < 66) return 6;
    return 7;
  }

  private hash(x: number, y: number): number {
    let h = (x * 374761393 + y * 668265263) >>> 0;
    h = ((h ^ (h >> 13)) * 1274126177) >>> 0;
    return (h ^ (h >> 16)) >>> 0;
  }

  private carvePath(map: number[][], x1: number, y1: number, x2: number, y2: number): void {
    const steps = Math.max(Math.abs(x2 - x1), Math.abs(y2 - y1)) * 3;
    for (let i = 0; i <= steps; i++) {
      const t = i / steps;
      const x = Math.round(x1 + (x2 - x1) * t);
      const y = Math.round(y1 + (y2 - y1) * t);
      // 2-tile wide path (cross shape)
      for (let dy = -1; dy <= 1; dy++) {
        for (let dx = -1; dx <= 1; dx++) {
          if (Math.abs(dx) + Math.abs(dy) > 1) continue; // skip corners for thinner path
          const mx = x + dx;
          const my = y + dy;
          if (mx >= 0 && mx < MAP_W && my >= 0 && my < MAP_H) {
            map[my][mx] = T.PATH;
          }
        }
      }
    }
  }

  // ── Rendering ───────────────────────────────────────

  private renderTerrain(map: number[][]): void {
    this.mapTiles = this.add.graphics();

    for (let y = 0; y < MAP_H; y++) {
      for (let x = 0; x < MAP_W; x++) {
        const terrain = map[y][x];
        let color = COLORS[terrain] ?? 0x000000;

        // Add subtle variation
        const v = (this.hash(x, y) % 16) - 8;
        color = this.adjustColor(color, v);

        this.mapTiles.fillStyle(color);
        this.mapTiles.fillRect(x * TILE, y * TILE, TILE, TILE);

        // Add detail for forests (small dark dots as "trees")
        if (terrain === T.FOREST) {
          this.mapTiles.fillStyle(0x1a4a1a);
          const ox = this.hash(x, y + 1) % 8;
          const oy = this.hash(x + 1, y) % 8;
          this.mapTiles.fillRect(x * TILE + ox, y * TILE + oy, 5, 5);
          this.mapTiles.fillStyle(0x1f5a1f);
          this.mapTiles.fillRect(x * TILE + ox + 1, y * TILE + oy, 3, 4);
        }

        // Mountains: small triangles
        if (terrain === T.MOUNTAIN) {
          this.mapTiles.fillStyle(0x776655);
          const px = x * TILE + 4;
          const py = y * TILE + 12;
          this.mapTiles.fillTriangle(px, py, px + 8, py, px + 4, py - 8);
          // Snow cap
          this.mapTiles.fillStyle(0xddddee);
          this.mapTiles.fillTriangle(px + 2, py - 4, px + 6, py - 4, px + 4, py - 8);
        }

        // Water: subtle wave lines
        if (terrain === T.WATER && this.hash(x, y) % 5 === 0) {
          this.mapTiles.fillStyle(0x4477bb);
          this.mapTiles.fillRect(x * TILE + 2, y * TILE + 6, 8, 1);
        }

        // Lava: glow dots
        if (terrain === T.LAVA && this.hash(x, y) % 4 === 0) {
          this.mapTiles.fillStyle(0xff6622);
          this.mapTiles.fillRect(x * TILE + 4, y * TILE + 4, 3, 3);
        }
      }
    }
  }

  private adjustColor(color: number, amount: number): number {
    let r = (color >> 16) & 0xff;
    let g = (color >> 8) & 0xff;
    let b = color & 0xff;
    r = Math.max(0, Math.min(255, r + amount));
    g = Math.max(0, Math.min(255, g + amount));
    b = Math.max(0, Math.min(255, b + amount));
    return (r << 16) | (g << 8) | b;
  }

  private renderPath(): void {
    // Draw path border/outline for visibility
    const gfx = this.add.graphics();
    gfx.lineStyle(1, 0x665533, 0.5);

    for (let i = 0; i < PATH_WAYPOINTS.length - 1; i++) {
      const [x1, y1] = PATH_WAYPOINTS[i];
      const [x2, y2] = PATH_WAYPOINTS[i + 1];
      gfx.lineBetween(
        x1 * TILE + TILE / 2,
        y1 * TILE + TILE / 2,
        x2 * TILE + TILE / 2,
        y2 * TILE + TILE / 2
      );
    }
  }

  private renderEventMarkers(): void {
    // Place zone labels at key path points
    const zones = [
      { km: 0, name: "Meadowlands" },
      { km: 9, name: "Whispering Woods" },
      { km: 18, name: "Highlands" },
      { km: 28, name: "Stormcrag Pass" },
      { km: 37, name: "Darkmarsh" },
      { km: 47, name: "Kingdom of Ember" },
      { km: 56, name: "Ashlands" },
      { km: 66, name: "Dragon's Peak" },
    ];

    for (const zone of zones) {
      const [px, py] = this.kmToPixel(zone.km);
      this.add
        .text(px, py - 20, zone.name, {
          fontFamily: FONT,
          fontSize: "6px",
          color: "#ffffff",
          stroke: "#000000",
          strokeThickness: 2,
        })
        .setOrigin(0.5);
    }
  }

  // ── Path interpolation ──────────────────────────────

  private kmToPixel(km: number): [number, number] {
    // Find the two waypoints we're between
    if (km <= 0) {
      const [x, y] = PATH_WAYPOINTS[0];
      return [x * TILE + TILE / 2, y * TILE + TILE / 2];
    }
    if (km >= TOTAL_KM) {
      const [x, y] = PATH_WAYPOINTS[PATH_WAYPOINTS.length - 1];
      return [x * TILE + TILE / 2, y * TILE + TILE / 2];
    }

    for (let i = 0; i < PATH_WAYPOINTS.length - 1; i++) {
      const [x1, y1, km1] = PATH_WAYPOINTS[i];
      const [x2, y2, km2] = PATH_WAYPOINTS[i + 1];
      if (km >= km1 && km <= km2) {
        const t = (km - km1) / (km2 - km1);
        const px = (x1 + (x2 - x1) * t) * TILE + TILE / 2;
        const py = (y1 + (y2 - y1) * t) * TILE + TILE / 2;
        return [px, py];
      }
    }

    const [x, y] = PATH_WAYPOINTS[PATH_WAYPOINTS.length - 1];
    return [x * TILE + TILE / 2, y * TILE + TILE / 2];
  }

  // ── Player Sprites ──────────────────────────────────

  private createPlayerSprite(id: string, data: PlayerData): Phaser.GameObjects.Container {
    const [px, py] = this.kmToPixel(data.map_position_km);
    const isMe = id === this.playerId;

    // Body — small character (8x12 rectangle)
    const color = isMe ? 0xc4a35a : 0x5a9ec4;
    const body = this.add.rectangle(0, 0, 8, 12, color);
    body.setStrokeStyle(1, 0x000000);

    // Head
    const head = this.add.circle(0, -8, 4, isMe ? 0xffcc88 : 0x88ccff);
    head.setStrokeStyle(1, 0x000000);

    // Name
    const nameTag = this.add
      .text(0, -18, data.name, {
        fontFamily: FONT,
        fontSize: "5px",
        color: isMe ? "#c4a35a" : "#5a9ec4",
        stroke: "#000000",
        strokeThickness: 2,
      })
      .setOrigin(0.5);

    // Walk indicator dot
    const walkDot = this.add.circle(0, 10, 2, 0x44ff44);
    walkDot.setVisible(data.is_walking);

    const container = this.add.container(px, py, [body, head, nameTag, walkDot]);
    container.setData("walkDot", walkDot);
    container.setDepth(10);

    return container;
  }

  // ── Minimap ─────────────────────────────────────────

  private minimapScale = 0;
  private minimapX = 0;
  private minimapY = 0;
  private minimapW = 0;
  private minimapH = 0;
  private minimapPlayerDots: Phaser.GameObjects.Graphics | null = null;

  private createMinimap(terrain: number[][]): void {
    this.minimapW = 160;
    this.minimapH = 120;
    this.minimapX = GAME_WIDTH - this.minimapW - 10;
    this.minimapY = 10;
    this.minimapScale = this.minimapW / (MAP_W * TILE);

    // Background
    const bg = this.add.graphics();
    bg.setScrollFactor(0);
    bg.setDepth(100);
    bg.fillStyle(0x000000, 0.7);
    bg.fillRect(this.minimapX - 2, this.minimapY - 2, this.minimapW + 4, this.minimapH + 4);
    bg.lineStyle(1, 0x444444);
    bg.strokeRect(this.minimapX - 2, this.minimapY - 2, this.minimapW + 4, this.minimapH + 4);

    // Terrain (low-res)
    this.minimapGfx = this.add.graphics();
    this.minimapGfx.setScrollFactor(0);
    this.minimapGfx.setDepth(101);

    const stepX = Math.ceil(MAP_W / this.minimapW);
    const stepY = Math.ceil(MAP_H / this.minimapH);

    for (let my = 0; my < this.minimapH; my++) {
      for (let mx = 0; mx < this.minimapW; mx++) {
        const tx = Math.min(Math.floor(mx * MAP_W / this.minimapW), MAP_W - 1);
        const ty = Math.min(Math.floor(my * MAP_H / this.minimapH), MAP_H - 1);
        const t = terrain[ty][tx];
        this.minimapGfx.fillStyle(COLORS[t] ?? 0x000000);
        this.minimapGfx.fillRect(this.minimapX + mx, this.minimapY + my, 1, 1);
      }
    }

    // Path on minimap
    this.minimapGfx.lineStyle(1, 0xffcc66, 0.8);
    for (let i = 0; i < PATH_WAYPOINTS.length - 1; i++) {
      const [x1, y1] = PATH_WAYPOINTS[i];
      const [x2, y2] = PATH_WAYPOINTS[i + 1];
      this.minimapGfx.lineBetween(
        this.minimapX + (x1 / MAP_W) * this.minimapW,
        this.minimapY + (y1 / MAP_H) * this.minimapH,
        this.minimapX + (x2 / MAP_W) * this.minimapW,
        this.minimapY + (y2 / MAP_H) * this.minimapH,
      );
    }

    // Player dots layer
    this.minimapPlayerDots = this.add.graphics();
    this.minimapPlayerDots.setScrollFactor(0);
    this.minimapPlayerDots.setDepth(102);
  }

  private updateMinimap(): void {
    if (!this.minimapPlayerDots) return;
    this.minimapPlayerDots.clear();

    for (const [id, data] of this.players) {
      const [px, py] = this.kmToPixel(data.map_position_km);
      const mx = this.minimapX + (px / (MAP_W * TILE)) * this.minimapW;
      const my = this.minimapY + (py / (MAP_H * TILE)) * this.minimapH;
      const isMe = id === this.playerId;
      this.minimapPlayerDots.fillStyle(isMe ? 0xffcc00 : 0x44aaff);
      this.minimapPlayerDots.fillCircle(mx, my, isMe ? 3 : 2);
    }

    // Camera viewport rectangle
    const cam = this.cameras.main;
    const vx = this.minimapX + (cam.scrollX / (MAP_W * TILE)) * this.minimapW;
    const vy = this.minimapY + (cam.scrollY / (MAP_H * TILE)) * this.minimapH;
    const vw = (GAME_WIDTH / (MAP_W * TILE)) * this.minimapW;
    const vh = (GAME_HEIGHT / (MAP_H * TILE)) * this.minimapH;
    this.minimapPlayerDots.lineStyle(1, 0xffffff, 0.5);
    this.minimapPlayerDots.strokeRect(vx, vy, vw, vh);
  }

  // ── Data Loading & Sync ─────────────────────────────

  private async loadInitialState(): Promise<void> {
    const { data: players } = await supabase
      .from("players")
      .select("*")
      .eq("game_id", this.gameId);

    if (players) {
      for (const p of players) {
        this.players.set(p.id, p as PlayerData);
        const sprite = this.createPlayerSprite(p.id, p as PlayerData);
        this.playerSprites.set(p.id, sprite);
      }
    }

    const { data: events } = await supabase
      .from("events")
      .select("*")
      .eq("game_id", this.gameId)
      .order("at_km");

    if (events) {
      this.gameEvents = events as EventData[];
      this.renderGameEventMarkers();
    }

    // Jump camera to player position
    const myData = this.players.get(this.playerId);
    if (myData) {
      const [px, py] = this.kmToPixel(myData.map_position_km);
      this.cameras.main.scrollX = px - GAME_WIDTH / 2;
      this.cameras.main.scrollY = py - GAME_HEIGHT / 2;
    }
  }

  private renderGameEventMarkers(): void {
    for (const event of this.gameEvents) {
      const [px, py] = this.kmToPixel(event.at_km);

      let color = 0xffffff;
      let symbol = "?";
      switch (event.event_type) {
        case "boss":
          color = 0xff4444;
          symbol = "!";
          break;
        case "npc":
          color = 0x44ff44;
          symbol = "?";
          break;
        case "treasure":
          color = 0xffaa00;
          symbol = "$";
          break;
        case "shop":
          color = 0x44aaff;
          symbol = "S";
          break;
        case "hazard":
          color = 0xff8800;
          symbol = "~";
          break;
        case "story":
          color = 0xcc88ff;
          symbol = "*";
          break;
      }

      const alpha = event.status === "completed" ? 0.3 : 0.9;

      // Marker circle
      const marker = this.add.circle(px, py - 12, 5, color).setAlpha(alpha);
      marker.setStrokeStyle(1, 0x000000);

      this.add
        .text(px, py - 12, symbol, {
          fontFamily: FONT,
          fontSize: "6px",
          color: "#000000",
        })
        .setOrigin(0.5)
        .setAlpha(alpha);

      // Event name
      this.add
        .text(px, py - 22, event.name, {
          fontFamily: FONT,
          fontSize: "4px",
          color: "#cccccc",
          stroke: "#000000",
          strokeThickness: 1,
        })
        .setOrigin(0.5)
        .setAlpha(alpha * 0.8);
    }
  }

  private subscribeToUpdates(): void {
    supabase
      .channel("players")
      .on(
        "postgres_changes",
        {
          event: "UPDATE",
          schema: "public",
          table: "players",
          filter: `game_id=eq.${this.gameId}`,
        },
        (payload) => {
          const data = payload.new as PlayerData;
          this.players.set(data.id, data);

          const sprite = this.playerSprites.get(data.id);
          if (sprite) {
            const indicator = sprite.getData("walkDot") as Phaser.GameObjects.Arc;
            indicator?.setVisible(data.is_walking);
          }

          // Update HUD
          this.scene.get("HUDScene")?.events.emit("playerUpdate", data);
        }
      )
      .subscribe();

    supabase
      .channel("events")
      .on(
        "postgres_changes",
        {
          event: "UPDATE",
          schema: "public",
          table: "events",
          filter: `game_id=eq.${this.gameId}`,
        },
        (payload) => {
          const data = payload.new as EventData;
          const idx = this.gameEvents.findIndex((e) => e.id === data.id);
          if (idx >= 0) {
            this.gameEvents[idx] = data;
          }
        }
      )
      .subscribe();

    supabase
      .channel("bosses")
      .on(
        "postgres_changes",
        {
          event: "*",
          schema: "public",
          table: "boss_encounters",
          filter: `game_id=eq.${this.gameId}`,
        },
        (payload) => {
          this.scene.get("HUDScene")?.events.emit("bossUpdate", payload.new);
        }
      )
      .subscribe();
  }

  private startHeartbeat(): void {
    if (!this.playerId) return;
    supabase.rpc("browser_heartbeat", { p_player_id: this.playerId });
    this.time.addEvent({
      delay: 10000,
      loop: true,
      callback: () => {
        supabase.rpc("browser_heartbeat", { p_player_id: this.playerId });
      },
    });
  }
}
