import Phaser from "phaser";

export class BootScene extends Phaser.Scene {
  constructor() {
    super({ key: "BootScene" });
  }

  preload(): void {
    // Show loading progress
    const width = this.cameras.main.width;
    const height = this.cameras.main.height;

    const progressBar = this.add.graphics();
    const progressBox = this.add.graphics();
    progressBox.fillStyle(0x222222, 0.8);
    progressBox.fillRect(width / 2 - 160, height / 2 - 15, 320, 30);

    const loadingText = this.add
      .text(width / 2, height / 2 - 40, "Loading...", {
        fontFamily: '"Press Start 2P", monospace',
        fontSize: "16px",
        color: "#ffffff",
      })
      .setOrigin(0.5);

    this.load.on("progress", (value: number) => {
      progressBar.clear();
      progressBar.fillStyle(0xc4a35a, 1);
      progressBar.fillRect(width / 2 - 155, height / 2 - 10, 310 * value, 20);
    });

    this.load.on("complete", () => {
      progressBar.destroy();
      progressBox.destroy();
      loadingText.destroy();
    });

    // TODO: Load actual assets here in Phase 6
    // this.load.image('tileset', 'assets/tilesets/...');
    // this.load.tilemapTiledJSON('map', 'assets/maps/...');
    // this.load.spritesheet('knight', 'assets/sprites/...', { frameWidth: 16, frameHeight: 16 });
  }

  create(): void {
    this.scene.start("TitleScene");
  }
}
