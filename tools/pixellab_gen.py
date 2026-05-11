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


def get_json(endpoint: str, token: str) -> dict:
    """GET helper for the background-jobs poll. Same error envelope as
    post_json so the caller can treat them uniformly."""
    url = f"{API_BASE}{endpoint}"
    req = urllib.request.Request(
        url,
        headers={"Authorization": f"Bearer {token}"},
        method="GET",
    )
    try:
        with urllib.request.urlopen(req, timeout=TIMEOUT_S) as resp:
            return json.loads(resp.read().decode())
    except urllib.error.HTTPError as e:
        body_txt = e.read().decode(errors="replace") if hasattr(e, "read") else ""
        return {"success": False, "error": f"HTTP {e.code}: {body_txt[:300]}"}
    except Exception as e:
        return {"success": False, "error": f"{type(e).__name__}: {e}"}


def get_balance(token: str) -> dict:
    """Fetch /balance — used to print credits used this run."""
    return get_json("/balance", token)


def extract_credits(resp: dict) -> Optional[float]:
    """Find a credits-remaining number in the /balance response. Tries a
    few likely paths since the docs don't pin the shape — falls back to
    None and the caller silently skips reporting."""
    if not isinstance(resp, dict):
        return None
    # Walk a small set of likely paths.
    paths = [
        ("data", "remaining_credits"),
        ("data", "credits"),
        ("data", "balance"),
        ("data", "remaining_generations"),
        ("remaining_credits",),
        ("credits",),
        ("remaining_generations",),
        ("balance",),
    ]
    for path in paths:
        v: object = resp
        for k in path:
            if not isinstance(v, dict):
                v = None
                break
            v = v.get(k)
        if isinstance(v, (int, float)):
            return float(v)
    return None


def poll_job(job_id: str, token: str, max_wait_s: int = 600, debug: bool = False) -> dict:
    """Poll /background-jobs/{id} until terminal status. Returns the
    last response (whether success or failure). Pixellab's POST endpoints
    return `{background_job_id, status: "processing"}` and do the actual
    image generation off-thread, so this turns the async API into a
    blocking one for the script's purposes."""
    import time
    interval = 2.0
    deadline = time.time() + max_wait_s
    last_status = None
    while time.time() < deadline:
        resp = get_json(f"/background-jobs/{job_id}", token)
        if resp.get("success") is False and resp.get("error"):
            return resp
        status = resp.get("status")
        if debug and status != last_status:
            print(f"\n  [poll] status={status}", end="", flush=True)
            last_status = status
        # Terminal states — done either way; let the extractor decide.
        if status in ("complete", "completed", "done", "succeeded", "success", "finished"):
            return resp
        if status in ("failed", "error", "cancelled", "canceled"):
            return resp
        time.sleep(interval)
    return {"success": False, "error": f"poll timeout after {max_wait_s}s on job {job_id}"}


def extract_image_payload(data: dict) -> Optional[dict]:
    """Return `{type, width?, base64}` for the first image-like record
    found in the response, or None. Always wraps bare base64 strings
    as `{type: "base64", ...}` so callers don't need to distinguish.
    Bytes vs. PNG distinction matters: `type: "rgba_bytes"` requires
    Pillow encoding (raw pixel bytes); `type: "base64"` is already
    a PNG ready to write."""
    if not isinstance(data, dict):
        return None
    # Direct payload at this level (e.g. /tilesets sometimes returns
    # one big {type, width, base64} blob).
    if isinstance(data.get("base64"), str) and len(data["base64"]) > 100:
        return data
    # Direct base64 strings under common keys.
    for key in ("image", "image_b64", "image_base64", "png", "content"):
        v = data.get(key)
        if isinstance(v, str) and len(v) > 100 and not v.startswith(("http://", "https://")):
            return {"type": "base64", "base64": v}
        if isinstance(v, dict):
            r = extract_image_payload(v)
            if r:
                return r
    # `images` may be a list (job result) or a dict keyed by direction
    # (character_4dir before direction-specific extraction).
    images = data.get("images")
    if isinstance(images, list) and images:
        first = images[0]
        if isinstance(first, str) and len(first) > 100:
            return {"type": "base64", "base64": first}
        if isinstance(first, dict):
            return extract_image_payload(first)
    if isinstance(images, dict):
        for v in images.values():
            if isinstance(v, dict):
                r = extract_image_payload(v)
                if r:
                    return r
    return None


# Back-compat shim for any caller that still wants just the base64
# string. Returns None for rgba_bytes (which has no useful "single
# string" representation without width / encoding).
def extract_image_b64(data: dict) -> Optional[str]:
    payload = extract_image_payload(data)
    if payload and payload.get("type", "base64") == "base64":
        return payload.get("base64")
    return None


def extract_image_url(data: dict) -> Optional[str]:
    """Some endpoints return an image URL instead of inline base64.
    We fetch it ourselves before saving."""
    if not isinstance(data, dict):
        return None
    for key in ("url", "image_url", "png_url", "asset_url", "src"):
        v = data.get(key)
        if isinstance(v, str) and v.startswith(("http://", "https://")):
            return v
    return None


def fetch_and_write(url: str, out_path: pathlib.Path) -> int:
    out_path.parent.mkdir(parents=True, exist_ok=True)
    req = urllib.request.Request(url)
    with urllib.request.urlopen(req, timeout=TIMEOUT_S) as resp:
        body = resp.read()
    out_path.write_bytes(body)
    return len(body)


def extract_direction_images(data: dict) -> Optional[dict]:
    """For /create-character-with-4-directions the API returns
    `images: {north: {...}, south: {...}, east: {...}, west: {...}}`,
    each entry being `{type, width, base64}` where `type` is either
    `"base64"` (PNG already) or `"rgba_bytes"` (raw pixel data we
    have to encode via Pillow). Returns a {direction: image_dict}
    dict, where direction is the compass key as-is."""
    if not isinstance(data, dict):
        return None
    images = data.get("images") or data.get("directions")
    if not isinstance(images, dict):
        return None
    out = {}
    for d in ("north", "south", "east", "west", "down", "up", "left", "right"):
        v = images.get(d)
        if isinstance(v, dict) and isinstance(v.get("base64"), str):
            out[d] = v
        elif isinstance(v, str) and len(v) > 100:
            # Bare base64 string (older shape).
            out[d] = {"type": "base64", "base64": v}
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
    # Pixellab's /tilesets is Wang-style: it generates the transition
    # tiles between two terrains, so it needs both descriptions.
    # Manifest can supply lower_description / upper_description
    # explicitly; falls back to splitting `description` on " over "
    # ("X over Y") or duplicating it as a last resort.
    lower = asset.get("lower_description")
    upper = asset.get("upper_description")
    if not lower or not upper:
        desc = asset.get("description", "")
        if " over " in desc:
            up, low = desc.split(" over ", 1)
            upper = upper or up
            lower = lower or low
        else:
            upper = upper or desc
            lower = lower or desc
    # tile_size is a {width, height} object on /tilesets, unlike the
    # bare int we'd guess from the manifest summary. Manifest can
    # supply an int (square) or [w, h] list — we normalize either way.
    raw_size = asset.get("tile_size", 16)
    if isinstance(raw_size, int):
        tile_size = {"width": raw_size, "height": raw_size}
    elif isinstance(raw_size, list) and len(raw_size) == 2:
        tile_size = {"width": raw_size[0], "height": raw_size[1]}
    else:
        tile_size = raw_size  # already an object — pass through
    body = {
        "lower_description": lower,
        "upper_description": upper,
        "tile_size": tile_size,
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


def _rgba_to_png(raw: bytes, width: int, height: int) -> bytes:
    """Wrap raw RGBA pixel bytes in a PNG file. Lazy-imports Pillow so
    the script still parses for users who haven't installed it; only
    rgba_bytes outputs (currently character_4dir) need this path."""
    try:
        from PIL import Image
    except ImportError as e:
        raise RuntimeError(
            "rgba_bytes output requires Pillow. Install: pip install Pillow"
        ) from e
    import io
    img = Image.frombytes("RGBA", (width, height), raw)
    buf = io.BytesIO()
    img.save(buf, format="PNG")
    return buf.getvalue()


PNG_SIGNATURE = b'\x89PNG\r\n\x1a\n'


def write_image_dict(img: dict, out_path: pathlib.Path) -> int:
    """Save one image given as a {type, width?, base64} dict. Detects
    actual content via the first bytes rather than trusting `type` —
    pixellab returns `type: "base64"` for both real PNGs and raw
    RGBA pixel bytes, so the type label can't be relied on."""
    out_path.parent.mkdir(parents=True, exist_ok=True)
    img_type = img.get("type", "base64")
    raw = base64.b64decode(img["base64"])

    # If the bytes start with the PNG magic, write directly. Otherwise
    # treat as raw RGBA — infer width from the dict, falling back to a
    # square if absent (works for our 128×128 wang tilesets where
    # pixellab omits width on the response).
    if raw.startswith(PNG_SIGNATURE) and img_type != "rgba_bytes":
        out_path.write_bytes(raw)
        return out_path.stat().st_size

    width = int(img.get("width") or 0)
    if width <= 0:
        import math
        pixels = len(raw) // 4
        side = math.isqrt(pixels)
        if side * side * 4 == len(raw):
            width = side
        else:
            raise RuntimeError(
                f"can't infer dimensions: {len(raw)} bytes, no width hint, "
                f"and not a square RGBA image"
            )
    if len(raw) % (4 * width) != 0:
        raise RuntimeError(
            f"rgba_bytes: bad width {width} for {len(raw)} bytes "
            f"(remainder {len(raw) % (4 * width)})"
        )
    height = len(raw) // (4 * width)
    png = _rgba_to_png(raw, width, height)
    out_path.write_bytes(png)
    return out_path.stat().st_size


def write_directions(directions: dict, out_path: pathlib.Path) -> int:
    """Save 4-direction output as 4 PNGs adjacent to `out_path`. Atlas
    stitching into our 4-row monster format is a separate step. Each
    direction entry is `{type, width, base64}`; pixellab uses compass
    keys (north/south/east/west) — saved as `<name>_<dir>.png`."""
    out_path.parent.mkdir(parents=True, exist_ok=True)
    stem = out_path.stem
    total = 0
    for d, img in directions.items():
        sibling = out_path.with_name(f"{stem}_{d}.png")
        total += write_image_dict(img, sibling)
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
    # Snapshot credits at start so we can report delta at end. Best-
    # effort — if /balance returns an unexpected shape we just skip
    # the report rather than fail the run.
    initial_credits = None
    if not args.dry_run:
        bal = get_balance(token)
        if args.debug:
            print("[balance] initial:", json.dumps(bal, default=str)[:300])
        initial_credits = extract_credits(bal)
        if initial_credits is not None:
            print(f"[balance] {int(initial_credits)} credits available")

    for asset in assets:
        out_rel = asset.get("out")
        if not out_rel:
            print(f"[error] {asset.get('id', '<no id>')}: missing 'out'")
            n_fail += 1
            continue
        out_path = out_root / out_rel
        # character_4dir saves 4 sibling files (`<stem>_<dir>.png`) and
        # never writes the bare out_path. Look for those instead so a
        # second run actually skips a previously-generated character.
        if asset.get("type") == "character_4dir":
            stem = out_path.stem
            siblings = [
                out_path.with_name(f"{stem}_{d}.png")
                for d in ("north", "south", "east", "west")
            ]
            existing = [p for p in siblings if p.exists()]
            if len(existing) >= 4 and not args.force:
                print(f"[skip ] {asset['id']:<22} → {out_path.with_name(stem + '_*.png')} (4/4 dirs exist)")
                n_skip += 1
                continue
        elif out_path.exists() and not args.force:
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

        # Pixellab's image endpoints kick off background jobs and
        # return {background_job_id, status: "processing"} immediately.
        # Block here, polling /background-jobs/{id} until terminal.
        job_id = resp.get("background_job_id") if isinstance(resp, dict) else None
        if job_id:
            print(f"job {job_id[:8]}...", end="", flush=True)
            resp = poll_job(job_id, token, debug=args.debug)

        # Always dump raw response under --debug so we can iterate on
        # the extractor when an endpoint returns an unexpected shape.
        if args.debug:
            print()
            dbg = json.dumps(resp, indent=2, default=str)
            if len(dbg) > 2500:
                dbg = dbg[:2500] + "\n  ... (truncated)"
            print("  raw response:\n  " + dbg.replace("\n", "\n  "))

        # Hard error path: HTTP 4xx/5xx or network exception (only our
        # post_json wrapper sets {success: False, error: "..."} like that).
        if resp.get("success") is False and resp.get("error"):
            print(f"FAIL — {resp['error']}")
            n_fail += 1
            continue

        # Try multiple paths to find the image. Pixellab's job responses
        # wrap it under `last_response.images[]`; older endpoints may
        # use `data.*` or top-level keys.
        candidates = [resp]
        if isinstance(resp.get("last_response"), dict):
            candidates.append(resp["last_response"])
        if isinstance(resp.get("data"), dict):
            candidates.append(resp["data"])
        if isinstance(resp.get("result"), dict):
            candidates.append(resp["result"])

        if asset["type"] == "character_4dir":
            dirs = None
            for c in candidates:
                dirs = extract_direction_images(c)
                if dirs:
                    break
            if dirs:
                size = write_directions(dirs, out_path)
                print(f"OK ({len(dirs)} directions, {size} bytes total)")
                n_done += 1
                continue
            # Fall through to single-image handling.

        payload = None
        img_url = None
        for c in candidates:
            payload = extract_image_payload(c)
            if payload:
                break
            img_url = extract_image_url(c)
            if img_url:
                break

        if not payload and not img_url:
            top_keys = list(resp.keys()) if isinstance(resp, dict) else []
            data_keys = list(resp.get("data", {}).keys()) if isinstance(resp.get("data"), dict) else []
            print(f"FAIL — no image found. top: {top_keys[:8]} data: {data_keys[:8]}")
            if not args.debug:
                snippet = json.dumps(resp, default=str)[:400]
                print(f"  snippet: {snippet} (re-run with --debug for full body)")
            n_fail += 1
            continue

        try:
            if payload:
                # write_image_dict handles BOTH `type: "base64"` (PNG)
                # and `type: "rgba_bytes"` (raw → PIL → PNG). Routing
                # all single-image saves through here means tileset
                # output gets correctly encoded same as character dirs.
                size = write_image_dict(payload, out_path)
            else:
                size = fetch_and_write(img_url, out_path)
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
    # Report credit usage for this run by re-querying balance.
    if not args.dry_run and initial_credits is not None:
        bal = get_balance(token)
        final_credits = extract_credits(bal)
        if final_credits is not None:
            used = int(initial_credits - final_credits)
            print(f"Credits: {int(final_credits)} remaining (used {used} this run)")
    return 0 if n_fail == 0 else 1


if __name__ == "__main__":
    raise SystemExit(main())
