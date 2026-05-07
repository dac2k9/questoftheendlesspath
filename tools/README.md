# `tools/` — developer utilities

## `pixellab_gen.py`

Manifest-driven pixel-art generation via the [pixellab.ai](https://pixellab.ai)
API. Reads `assets/manifest.yaml`, calls the API for each entry that
isn't already on disk, and saves PNGs under
`crates/gameclient/assets/generated/`.

### Setup (one time)

```bash
# 1. Get a token from https://pixellab.ai/account
export PIXEL_LAB_TOKEN=...   # paste your token here

# 2. Install dependencies (PyYAML for the manifest, Pillow for
#    rgba_bytes → PNG encoding used by character_4dir output)
pip install pyyaml Pillow
```

### Generate

```bash
# Run everything in the manifest (skips assets already on disk)
python tools/pixellab_gen.py

# See what would happen without spending credits
python tools/pixellab_gen.py --dry-run

# Just one asset
python tools/pixellab_gen.py --only bog_wraith

# Re-roll an existing asset (overwrites)
python tools/pixellab_gen.py --only bog_wraith --force

# When something fails, see the raw API response
python tools/pixellab_gen.py --only bog_wraith --debug
```

### Manifest format

`assets/manifest.yaml`:

```yaml
defaults:
  out_root: crates/gameclient/assets/generated

assets:
  - id: bog_wraith                      # local handle (used for --only)
    type: character_4dir                # see "type → endpoint" below
    description: "ghostly pale-green swamp wraith, fantasy pixel art"
    size: 64                            # int → square; or [w, h]
    out: monsters/BogWraith.png         # under out_root
```

`type` → pixellab endpoint:

| `type`           | Endpoint                                  | Notes                              |
| ---------------- | ----------------------------------------- | ---------------------------------- |
| `character_4dir` | `/create-character-with-4-directions`     | Returns 4 directions or one sheet  |
| `image`          | `/generate-image-v2`                      | Generic single image               |
| `object`         | `/generate-image-v2`                      | Add "transparent background" to description |
| `tileset`        | `/tilesets`                               | `tile_size: 16` or `32`            |

### 4-direction outputs

When the API returns 4 separate per-direction PNGs (Down/Up/Left/Right),
the script saves them as `<name>_down.png`, `<name>_up.png`, etc.
Stitching into our atlas format (4 rows × N walk-cycle columns at
16×16 each) is a separate post-processing step we'll add once we see
the actual API output and pick a layout we're happy with.
