import Phaser from "phaser";
import { supabase, GAME_WIDTH, GAME_HEIGHT } from "../config";

export class HUDScene extends Phaser.Scene {
  private speedText!: Phaser.GameObjects.Text;
  private goldText!: Phaser.GameObjects.Text;
  private positionText!: Phaser.GameObjects.Text;
  private statusText!: Phaser.GameObjects.Text;
  private bossBar?: Phaser.GameObjects.Graphics;
  private bossText?: Phaser.GameObjects.Text;

  private playerId = "";
  private gameId = "";

  constructor() {
    super({ key: "HUDScene" });
  }

  create(): void {
    this.playerId = this.registry.get("playerId") as string;
    this.gameId = this.registry.get("gameId") as string;
    const playerName = (this.registry.get("playerName") as string) || "Adventurer";

    // Top-left: Player name
    this.add
      .text(10, 10, playerName, {
        fontFamily: '"Press Start 2P", monospace',
        fontSize: "14px",
        color: "#c4a35a",
      })
      .setScrollFactor(0);

    // Top-left: Speed (below name)
    this.speedText = this.add
      .text(10, 30, "Speed: 0.0 km/h", {
        fontFamily: '"Press Start 2P", monospace',
        fontSize: "14px",
        color: "#c4a35a",
      })
      .setScrollFactor(0);

    // Top-left: Position
    this.positionText = this.add
      .text(10, 50, "Position: 0.0 / 75.0 km", {
        fontFamily: '"Press Start 2P", monospace',
        fontSize: "12px",
        color: "#ffffff",
      })
      .setScrollFactor(0);

    // Top-right: Gold
    this.goldText = this.add
      .text(GAME_WIDTH - 10, 10, "Gold: 0", {
        fontFamily: '"Press Start 2P", monospace',
        fontSize: "14px",
        color: "#ffaa00",
      })
      .setOrigin(1, 0)
      .setScrollFactor(0);

    // Top-center: Status
    this.statusText = this.add
      .text(GAME_WIDTH / 2, 10, "", {
        fontFamily: '"Press Start 2P", monospace',
        fontSize: "12px",
        color: "#ff4444",
        align: "center",
      })
      .setOrigin(0.5, 0)
      .setScrollFactor(0);

    // Bottom: Progress bar
    this.drawProgressBar(0, 75);

    // Listen for player updates from GameScene
    this.events.on("playerUpdate", (data: Record<string, unknown>) => {
      if (data.id === this.playerId) {
        this.updateHUD(data);
      }
    });

    // Listen for boss updates
    this.events.on("bossUpdate", (data: Record<string, unknown>) => {
      this.updateBossBar(data);
    });

    // Poll for initial state
    this.loadPlayerState();
  }

  private async loadPlayerState(): Promise<void> {
    const { data } = await supabase
      .from("players")
      .select("*")
      .eq("id", this.playerId)
      .single();

    if (data) {
      this.updateHUD(data);
    }
  }

  private updateHUD(data: Record<string, unknown>): void {
    const speed = (data.current_speed_kmh as number) || 0;
    const position = (data.map_position_km as number) || 0;
    const gold = (data.gold as number) || 0;
    const isBlocked = data.is_blocked as boolean;
    const isWalking = data.is_walking as boolean;

    this.speedText.setText(`Speed: ${speed.toFixed(1)} km/h`);
    this.speedText.setColor(isWalking ? "#44ff44" : "#c4a35a");

    this.positionText.setText(`Position: ${position.toFixed(1)} / 75.0 km`);
    this.goldText.setText(`Gold: ${gold}`);

    if (isBlocked) {
      this.statusText.setText("Waiting for party at gate...");
    } else if (!isWalking) {
      this.statusText.setText("");
    } else {
      this.statusText.setText("");
    }

    this.drawProgressBar(position, 75);
  }

  private drawProgressBar(position: number, total: number): void {
    const barWidth = GAME_WIDTH - 40;
    const barHeight = 12;
    const barX = 20;
    const barY = GAME_HEIGHT - 25;

    // Clear previous
    const existing = this.children.getByName("progressBar");
    if (existing) existing.destroy();

    const graphics = this.add.graphics();
    graphics.setName("progressBar");

    // Background
    graphics.fillStyle(0x222222, 0.8);
    graphics.fillRect(barX, barY, barWidth, barHeight);

    // Fill
    const fillWidth = (position / total) * barWidth;
    graphics.fillStyle(0xc4a35a, 1);
    graphics.fillRect(barX, barY, fillWidth, barHeight);

    // Zone ticks
    const zoneBoundaries = [9, 18, 28, 37, 47, 56, 66];
    graphics.lineStyle(1, 0xffffff, 0.3);
    for (const km of zoneBoundaries) {
      const x = barX + (km / total) * barWidth;
      graphics.lineBetween(x, barY, x, barY + barHeight);
    }

    // Border
    graphics.lineStyle(1, 0x444444);
    graphics.strokeRect(barX, barY, barWidth, barHeight);
  }

  private updateBossBar(data: Record<string, unknown>): void {
    const currentHp = (data.current_hp as number) || 0;
    const maxHp = (data.max_hp as number) || 1;
    const defeated = data.defeated as boolean;
    const bossName = (data.boss_name as string) || "Boss";

    if (defeated) {
      this.bossBar?.destroy();
      this.bossText?.destroy();
      this.bossBar = undefined;
      this.bossText = undefined;
      return;
    }

    const barWidth = 300;
    const barX = (GAME_WIDTH - barWidth) / 2;
    const barY = 60;

    if (!this.bossBar) {
      this.bossBar = this.add.graphics();
    }
    this.bossBar.clear();

    // Background
    this.bossBar.fillStyle(0x440000, 0.9);
    this.bossBar.fillRect(barX, barY, barWidth, 20);

    // HP fill
    const fillWidth = (currentHp / maxHp) * barWidth;
    this.bossBar.fillStyle(0xff2222, 1);
    this.bossBar.fillRect(barX, barY, fillWidth, 20);

    // Border
    this.bossBar.lineStyle(2, 0xff4444);
    this.bossBar.strokeRect(barX, barY, barWidth, 20);

    if (!this.bossText) {
      this.bossText = this.add
        .text(GAME_WIDTH / 2, barY - 5, "", {
          fontFamily: '"Press Start 2P", monospace',
          fontSize: "12px",
          color: "#ff4444",
        })
        .setOrigin(0.5, 1);
    }
    this.bossText.setText(`${bossName} — ${currentHp}/${maxHp} HP`);
  }
}
