import Phaser from "phaser";
import { supabase, GAME_WIDTH, GAME_HEIGHT } from "../config";

const FONT = '"Press Start 2P", monospace';

interface PlayerRecord {
  id: string;
  name: string;
  avatar: string;
}

export class TitleScene extends Phaser.Scene {
  private joinCodeInput = "";
  private phase: "code" | "name" = "code";
  private players: PlayerRecord[] = [];
  private selectedIndex = 0;
  private nameTexts: Phaser.GameObjects.Text[] = [];

  constructor() {
    super({ key: "TitleScene" });
  }

  create(): void {
    this.phase = "code";
    this.joinCodeInput = "";
    this.drawTitle();
    this.drawCodeInput();

    this.input.keyboard?.on("keydown", (event: KeyboardEvent) => {
      if (this.phase === "code") {
        this.handleCodeInput(event);
      } else if (this.phase === "name") {
        this.handleNameSelect(event);
      }
    });
  }

  private drawTitle(): void {
    const cx = GAME_WIDTH / 2;
    const cy = GAME_HEIGHT / 2;

    this.add
      .text(cx, cy - 140, "Quest of the\nEndless Path", {
        fontFamily: FONT,
        fontSize: "32px",
        color: "#c4a35a",
        align: "center",
      })
      .setOrigin(0.5);

    this.add
      .text(cx, cy - 70, "A cooperative treadmill adventure", {
        fontFamily: FONT,
        fontSize: "10px",
        color: "#888888",
      })
      .setOrigin(0.5);
  }

  // ── Phase 1: Join Code ──────────────────────────────

  private codeDisplay?: Phaser.GameObjects.Text;
  private codeHint?: Phaser.GameObjects.Text;

  private drawCodeInput(): void {
    const cx = GAME_WIDTH / 2;
    const cy = GAME_HEIGHT / 2;

    this.codeDisplay = this.add
      .text(cx, cy + 10, "Join code: ______", {
        fontFamily: FONT,
        fontSize: "16px",
        color: "#ffffff",
      })
      .setOrigin(0.5);

    this.codeHint = this.add
      .text(cx, cy + 50, "Type code, press ENTER", {
        fontFamily: FONT,
        fontSize: "10px",
        color: "#666666",
      })
      .setOrigin(0.5);
  }

  private handleCodeInput(event: KeyboardEvent): void {
    if (event.key === "Backspace") {
      this.joinCodeInput = this.joinCodeInput.slice(0, -1);
    } else if (event.key === "Enter" && this.joinCodeInput.length >= 4) {
      this.lookupGame(this.joinCodeInput);
      return;
    } else if (
      event.key.length === 1 &&
      this.joinCodeInput.length < 6 &&
      /[a-zA-Z0-9]/.test(event.key)
    ) {
      this.joinCodeInput += event.key.toUpperCase();
    }

    const padded = this.joinCodeInput.padEnd(6, "_");
    this.codeDisplay?.setText(`Join code: ${padded}`);
  }

  private async lookupGame(code: string): Promise<void> {
    const { data: game, error } = await supabase
      .from("games")
      .select("*")
      .eq("join_code", code)
      .single();

    if (error || !game) {
      this.showError("Game not found!");
      return;
    }

    this.registry.set("gameId", game.id);
    this.registry.set("joinCode", code);

    // Fetch players in this game
    const { data: players } = await supabase
      .from("players")
      .select("id, name, avatar")
      .eq("game_id", game.id);

    if (!players || players.length === 0) {
      this.showError("No players in this game!");
      return;
    }

    this.players = players as PlayerRecord[];

    // Transition to name selection
    this.codeDisplay?.destroy();
    this.codeHint?.destroy();
    this.phase = "name";
    this.selectedIndex = 0;
    this.drawNameSelect();
  }

  // ── Phase 2: Name Selection ─────────────────────────

  private drawNameSelect(): void {
    const cx = GAME_WIDTH / 2;
    const cy = GAME_HEIGHT / 2;

    this.add
      .text(cx, cy - 10, "Who are you?", {
        fontFamily: FONT,
        fontSize: "14px",
        color: "#ffffff",
      })
      .setOrigin(0.5);

    this.nameTexts = [];
    this.players.forEach((player, i) => {
      const text = this.add
        .text(cx, cy + 25 + i * 30, player.name, {
          fontFamily: FONT,
          fontSize: "14px",
          color: i === 0 ? "#c4a35a" : "#666666",
        })
        .setOrigin(0.5);
      this.nameTexts.push(text);
    });

    this.add
      .text(cx, cy + 25 + this.players.length * 30 + 15, "Arrow keys to select, ENTER to confirm", {
        fontFamily: FONT,
        fontSize: "8px",
        color: "#444444",
      })
      .setOrigin(0.5);

    this.updateNameHighlight();
  }

  private handleNameSelect(event: KeyboardEvent): void {
    if (event.key === "ArrowUp") {
      this.selectedIndex = Math.max(0, this.selectedIndex - 1);
      this.updateNameHighlight();
    } else if (event.key === "ArrowDown") {
      this.selectedIndex = Math.min(this.players.length - 1, this.selectedIndex + 1);
      this.updateNameHighlight();
    } else if (event.key === "Enter") {
      this.confirmPlayer(this.players[this.selectedIndex]);
    }
  }

  private updateNameHighlight(): void {
    this.nameTexts.forEach((text, i) => {
      if (i === this.selectedIndex) {
        text.setColor("#c4a35a");
        text.setText(`> ${this.players[i].name} <`);
      } else {
        text.setColor("#666666");
        text.setText(this.players[i].name);
      }
    });
  }

  private confirmPlayer(player: PlayerRecord): void {
    this.registry.set("playerId", player.id);
    this.registry.set("playerName", player.name);

    // Remember for next time
    localStorage.setItem("playerId", player.id);
    localStorage.setItem("playerName", player.name);

    // Welcome message then transition
    const welcomeText = this.add
      .text(GAME_WIDTH / 2, GAME_HEIGHT / 2 + 130, `Welcome, ${player.name}!`, {
        fontFamily: FONT,
        fontSize: "14px",
        color: "#c4a35a",
      })
      .setOrigin(0.5);

    this.time.delayedCall(1000, () => {
      welcomeText.destroy();
      this.scene.start("GameScene");
      this.scene.launch("HUDScene");
    });
  }

  // ── Helpers ─────────────────────────────────────────

  private showError(msg: string): void {
    this.add
      .text(GAME_WIDTH / 2, GAME_HEIGHT / 2 + 130, msg, {
        fontFamily: FONT,
        fontSize: "12px",
        color: "#ff4444",
      })
      .setOrigin(0.5);
  }
}
