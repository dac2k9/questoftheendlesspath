#!/usr/bin/env python3
"""
Generate pixel art assets via the pixellab.ai API, driven by
`assets/manifest.yaml`.

Token comes from the env var `PIXEL_LAB_TOKEN`. By default we skip
assets whose output already exists on disk so re-running the script
doesn't burn credits regenerating finished work — pass `--force` to
regenerate everything, or `--only ID` to target one asset.

Response shapes vary by endpoint (single image vs. multi-direction
sheet vs. tileset frames). The extractor tries the common keys and
falls back to dumping the raw response for inspection on failure.
"""

import argparse
import base64
import json
import os
import pathlib
import sys
import urllib.error
import urllib.request
from typing import Optional

try:
    import yaml  # PyYAML; pip install pyyaml
except ImportError:
    print("ERROR: PyYAML required. Run: pip install pyyaml", file=sys.stderr)
    sys.exit(1)


API_BASE = "https://api.pixellab.ai/v2"
TIMEOUT_S = 180


def load_manifest(path: str) -> dict:
    with open(path) as f:
        return yaml.safe_load(f)


def post_json(endpoint: str, body: dict, token: str) -> dict:
    """POST a JSON body, return the parsed response (or an error dict
    with `success: False`). 4xx / 5xx errors are caught and returned
    in-band so the script can keep iterating through other assets."""
    url = f"{API_BASE}{endpoint}"
    req = urllib.request.Request(
        url,
        data=json.dumps(body).encode(),
        headers={
            "Authorization": f"Bearer {token}",
            "Content-Type": "application/json",
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=TIMEOUT_S) as resp:
            return json.loads(resp.read().decode())
    except urllib.error.HTTPError as e:
        body_txt = e.read().decode(errors="replace") if hasattr(e, "read") else ""
        return {"success": False, "error": f"HTTP {e.code}: {body_txt[:300]}"}
    except Exception as e:
        return {"success": False, "error": f"{type(e).__name__}: {e}"}


def extract_image_b64(data: dict) -> Optional[str]:
    """Try common keys where the API may have stuffed the base64 PNG.
    Pixellab's docs hint at `data.image` but variants exist across
    endpoints — this is best-effort, falls through to None if nothing
    looks like an image."""
    if not isinstance(data, dict):
        return None
    for key in ("image", "image_b64", "image_base64", "png", "data"):
        v = data.get(key)
        if isinstance(v, str) and len(v) > 100:
            return v
    return None


def extract_direction_images(data: dict) -> Optional[dict]:
    """For `/create-character-with-4-directions`, the API may return a
    dict like `{down: <b64>, up: <b64>, left: <b64>, right: <b64>}`
    or wrap them under `directions`. Returns a {direction: b64} dict
    if found, else None."""
    if not isinstance(data, dict):
        return None
    candidate = data.get("directions") or data
    if not isinstance(candidate, dict):
        return None
    out = {}
    for d in ("down", "up", "left", "right"):
        v = candidate.get(d)
        if isinstance(v, str) and len(v) > 100:
            out[d] = v
        elif isinstance(v, dict):
            inner = extract_image_b64(v)
            if inner:
                out[d] = inner
    return out if len(out) >= 2 else None


def gen_image(asset: dict, token: str) -> dict:
    size = asset.get("size", [64, 64])
    if isinstance(size, int):
        size = [size, size]
    body = {
        "description": asset["description"],
        "image_size": {"width": size[0], "height": size[1]},
    }
    return post_json("/generate-image-v2", body, token)


def gen_character_4dir(asset: dict, token: str) -> dict:
    size = asset.get("size", 64)
    if isinstance(size, list):
        size = size[0]
    body = {
        "description": asset["description"],
        "image_size": {"width": size, "height": size},
    }
    return post_json("/create-character-with-4-directions", body, token)


def gen_tileset(asset: dict, token: str) -> dict:
    body = {
        "description": asset["description"],
        "tile_size": asset.get("tile_size", 16),
    }
    return post_json("/tilesets", body, token)


GENERATORS = {
    "image": gen_image,
    "object": gen_image,  # transparent-bg phrasing in description
    "character_4dir": gen_character_4dir,
    "tileset": gen_tileset,
}


def write_png(b64: str, out_path: pathlib.Path) -> int:
    out_path.parent.mkdir(parents=True, exist_ok=True)
    raw = base64.b64decode(b64)
    out_path.write_bytes(raw)
    return len(raw)


def write_directions(directions: dict, out_path: pathlib.Path) -> int:
    """Save 4-direction output as 4 PNGs adjacent to `out_path`. Atlas
    stitching into our 4-row monster format is a separate step."""
    out_path.parent.mkdir(parents=True, exist_ok=True)
    stem = out_path.stem
    total = 0
    for d, b64 in directions.items():
        sibling = out_path.with_name(f"{stem}_{d}.png")
        sibling.write_bytes(base64.b64decode(b64))
        total += sibling.stat().st_size
    return total


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("manifest", nargs="?", default="assets/manifest.yaml")
    p.add_argument("--force", action="store_true", help="Regenerate even if output exists")
    p.add_argument("--only", help="Only generate the asset with this id")
    p.add_argument("--dry-run", action="store_true", help="Don't call the API; print the plan")
    p.add_argument("--debug", action="store_true", help="Print raw API responses on failure")
    args = p.parse_args()

    token = os.environ.get("PIXEL_LAB_TOKEN")
    if not token and not args.dry_run:
        print("ERROR: set PIXEL_LAB_TOKEN env var (token from https://pixellab.ai/account)", file=sys.stderr)
        return 2

    manifest_path = pathlib.Path(args.manifest)
    if not manifest_path.exists():
        print(f"ERROR: manifest not found: {manifest_path}", file=sys.stderr)
        return 2
    manifest = load_manifest(str(manifest_path))
    out_root = pathlib.Path(manifest.get("defaults", {}).get("out_root", "."))

    assets = manifest.get("assets", [])
    if args.only:
        assets = [a for a in assets if a.get("id") == args.only]
        if not assets:
            print(f"ERROR: no asset with id '{args.only}' in manifest", file=sys.stderr)
            return 2

    n_done, n_skip, n_fail = 0, 0, 0
    last_credits = None

    for asset in assets:
        out_rel = asset.get("out")
        if not out_rel:
            print(f"[error] {asset.get('id', '<no id>')}: missing 'out'")
            n_fail += 1
            continue
        out_path = out_root / out_rel
        if out_path.exists() and not args.force:
            print(f"[skip ] {asset['id']:<22} → {out_path} (exists; --force to regen)")
            n_skip += 1
            continue
        gen = GENERATORS.get(asset.get("type"))
        if not gen:
            print(f"[error] {asset.get('id')}: unknown type '{asset.get('type')}'")
            n_fail += 1
            continue
        if args.dry_run:
            print(f"[plan ] {asset['id']:<22} → {out_path}  ({asset['type']})")
            continue

        print(f"[gen  ] {asset['id']:<22} ({asset['type']})...", end=" ", flush=True)
        resp = gen(asset, token)
        if not resp.get("success"):
            print(f"FAIL — {resp.get('error', '?')}")
            n_fail += 1
            continue
        data = resp.get("data") or {}

        if asset["type"] == "character_4dir":
            dirs = extract_direction_images(data)
            if dirs:
                size = write_directions(dirs, out_path)
                print(f"OK ({len(dirs)} directions, {size} bytes total)")
                n_done += 1
                continue
            # Fall through to single-image handling — some endpoints
            # may return a single sheet rather than per-direction.

        img = extract_image_b64(data)
        if not img:
            print(
                f"FAIL — no image in response (data keys: {list(data.keys()) if isinstance(data, dict) else 'n/a'})"
            )
            if args.debug:
                print("  raw response:", json.dumps(resp, indent=2)[:1500])
            n_fail += 1
            continue
        try:
            size = write_png(img, out_path)
        except Exception as e:
            print(f"FAIL — write {out_path}: {e}")
            n_fail += 1
            continue
        print(f"OK ({size} bytes)")
        n_done += 1
        usage = resp.get("usage") or {}
        if "remaining_credits" in usage:
            last_credits = usage["remaining_credits"]

    print()
    print(f"Summary: {n_done} generated, {n_skip} skipped, {n_fail} failed")
    if last_credits is not None:
        print(f"Credits remaining (last reported): {last_credits}")
    return 0 if n_fail == 0 else 1


if __name__ == "__main__":
    raise SystemExit(main())
