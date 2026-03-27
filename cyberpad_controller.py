#!/usr/bin/env python3
"""
UREVO CyberPad FTMS Controller
Connect to the CyberPad via Bluetooth FTMS and control it.
"""

import asyncio
import struct
import sys
import signal
import logging
from bleak import BleakClient, BleakScanner

logging.basicConfig(level=logging.WARNING)
log = logging.getLogger("cyberpad")

# Default address — override with CLI arg
DEVICE_ADDRESS = "0C7A1C20-227B-0CE0-107E-E598A917FA11"

# FTMS UUIDs
FTMS_SERVICE          = "00001826-0000-1000-8000-00805f9b34fb"
TREADMILL_DATA        = "00002acd-0000-1000-8000-00805f9b34fb"
MACHINE_FEATURE       = "00002acc-0000-1000-8000-00805f9b34fb"
MACHINE_STATUS        = "00002ad6-0000-1000-8000-00805f9b34fb"
CONTROL_POINT         = "00002ad9-0000-1000-8000-00805f9b34fb"
SUPPORTED_SPEED       = "00002ad0-0000-1000-8000-00805f9b34fb"
SUPPORTED_INCLINE     = "00002ad1-0000-1000-8000-00805f9b34fb"
TRAINING_STATUS       = "00002ad8-0000-1000-8000-00805f9b34fb"

# FTMS Control Point OpCodes
OP_REQUEST_CONTROL    = 0x00
OP_RESET              = 0x01
OP_SET_SPEED          = 0x02  # uint16 in 0.01 km/h
OP_SET_INCLINE        = 0x03  # int16 in 0.1%
OP_START_RESUME       = 0x07
OP_STOP_PAUSE         = 0x08  # 0x01=stop, 0x02=pause
OP_RESPONSE           = 0x80

# State
current_speed = 0.0      # km/h
current_distance = 0     # meters
current_incline = 0.0    # %
current_steps = 0
current_elapsed = 0      # seconds
is_running = False


def parse_treadmill_data(sender, data: bytearray):
    """Parse FTMS Treadmill Data characteristic notification."""
    global current_speed, current_distance, current_incline, current_steps, current_elapsed, is_running

    if len(data) < 2:
        return

    # Flags field (16 bits)
    flags = struct.unpack_from("<H", data, 0)[0]
    offset = 2

    # Bit 0: Instantaneous Speed NOT present (0 = present)
    if not (flags & 0x0001):
        if offset + 2 <= len(data):
            raw_speed = struct.unpack_from("<H", data, offset)[0]
            current_speed = raw_speed * 0.01  # km/h
            is_running = current_speed > 0
            offset += 2

    # Bit 1: Average Speed present
    if flags & 0x0002:
        offset += 2

    # Bit 2: Total Distance present
    if flags & 0x0004:
        if offset + 3 <= len(data):
            current_distance = int.from_bytes(data[offset:offset+3], "little")
            offset += 3

    # Bit 3: Inclination and Ramp Angle present
    if flags & 0x0008:
        if offset + 4 <= len(data):
            raw_incline = struct.unpack_from("<h", data, offset)[0]
            current_incline = raw_incline * 0.1
            offset += 4  # incline (2) + ramp angle (2)

    # Bit 4: Elevation Gain present
    if flags & 0x0010:
        offset += 4

    # Bit 5: Instantaneous Pace present
    if flags & 0x0020:
        offset += 1

    # Bit 6: Average Pace present
    if flags & 0x0040:
        offset += 1

    # Bit 7: Expended Energy present
    if flags & 0x0080:
        offset += 5  # total(2) + per_hour(2) + per_min(1)

    # Bit 8: Heart Rate present
    if flags & 0x0100:
        offset += 1

    # Bit 9: Metabolic Equivalent present
    if flags & 0x0200:
        offset += 1

    # Bit 10: Elapsed Time present
    if flags & 0x0400:
        if offset + 2 <= len(data):
            current_elapsed = struct.unpack_from("<H", data, offset)[0]
            offset += 2

    # Bit 11: Remaining Time present
    if flags & 0x0800:
        offset += 2

    # Bit 12: Force on Belt / Power present
    if flags & 0x1000:
        offset += 4

    # Step count (not standard FTMS, but some devices include it)
    # We'll check for extra bytes


def parse_machine_status(sender, data: bytearray):
    """Parse FTMS Machine Status notifications."""
    if len(data) < 1:
        return
    opcode = data[0]
    status_names = {
        0x01: "Reset",
        0x02: "Stopped/Paused (stop)",
        0x03: "Stopped/Paused (pause)",
        0x04: "Started/Resumed",
        0x05: "Target Speed Changed",
        0x06: "Target Incline Changed",
        0x08: "Target Heart Rate Changed",
        0x0E: "Control Permission Lost",
        0xFF: "Control Permission Granted",
    }
    name = status_names.get(opcode, f"Unknown(0x{opcode:02x})")
    print(f"\r  [STATUS] {name} (raw: {data.hex()})")


def format_time(seconds):
    m, s = divmod(seconds, 60)
    h, m = divmod(m, 60)
    if h > 0:
        return f"{h}:{m:02d}:{s:02d}"
    return f"{m}:{s:02d}"


def print_dashboard():
    speed_bar = "=" * int(current_speed * 5)
    status = "RUNNING" if is_running else "STOPPED"
    print(
        f"\r  [{status}]  "
        f"Speed: {current_speed:5.2f} km/h [{speed_bar:<30}]  "
        f"Incline: {current_incline:4.1f}%  "
        f"Dist: {current_distance}m  "
        f"Time: {format_time(current_elapsed)}  ",
        end="",
        flush=True,
    )


async def write_control(client, opcode, payload=b""):
    """Write to FTMS Control Point."""
    data = bytes([opcode]) + payload
    try:
        await client.write_gatt_char(CONTROL_POINT, data, response=True)
        return True
    except Exception as e:
        print(f"\n  [ERROR] Control write failed: {e}")
        return False


async def request_control(client):
    print("  Requesting control...")
    if await write_control(client, OP_REQUEST_CONTROL):
        print("  Control granted!")
        return True
    return False


async def start_treadmill(client):
    print("\n  Starting treadmill...")
    await write_control(client, OP_START_RESUME)


async def stop_treadmill(client):
    print("\n  Stopping treadmill...")
    await write_control(client, OP_STOP_PAUSE, bytes([0x01]))


async def pause_treadmill(client):
    print("\n  Pausing treadmill...")
    await write_control(client, OP_STOP_PAUSE, bytes([0x02]))


async def set_speed(client, speed_kmh: float):
    """Set target speed in km/h."""
    raw = int(speed_kmh * 100)  # 0.01 km/h resolution
    payload = struct.pack("<H", raw)
    print(f"\n  Setting speed to {speed_kmh:.1f} km/h...")
    await write_control(client, OP_SET_SPEED, payload)


async def set_incline(client, incline_pct: float):
    """Set target incline in %.

    NOTE: The CyberPad physically stops when receiving an incline command
    while running. We work around this by automatically resuming at the
    previous speed after the incline change completes.
    """
    was_running = is_running
    prev_speed = current_speed

    raw = int(incline_pct * 10)  # 0.1% resolution
    payload = struct.pack("<h", raw)
    print(f"\n  Setting incline to {incline_pct:.1f}%...")
    await write_control(client, OP_SET_INCLINE, payload)

    if was_running and prev_speed > 0:
        # Give the treadmill a moment to process the incline change
        await asyncio.sleep(1.5)
        print(f"  Resuming at {prev_speed:.1f} km/h...")
        await write_control(client, OP_START_RESUME)
        await asyncio.sleep(0.5)
        raw_speed = int(prev_speed * 100)
        await write_control(client, OP_SET_SPEED, struct.pack("<H", raw_speed))


async def read_supported_ranges(client):
    """Read and display supported speed and incline ranges."""
    try:
        data = await client.read_gatt_char(SUPPORTED_SPEED)
        if len(data) >= 6:
            min_spd = struct.unpack_from("<H", data, 0)[0] * 0.01
            max_spd = struct.unpack_from("<H", data, 2)[0] * 0.01
            inc_spd = struct.unpack_from("<H", data, 4)[0] * 0.01
            print(f"  Speed range: {min_spd:.1f} - {max_spd:.1f} km/h (step: {inc_spd:.2f})")
    except Exception:
        print("  Speed range: not available")

    try:
        data = await client.read_gatt_char(SUPPORTED_INCLINE)
        if len(data) >= 6:
            min_inc = struct.unpack_from("<h", data, 0)[0] * 0.1
            max_inc = struct.unpack_from("<h", data, 2)[0] * 0.1
            inc_inc = struct.unpack_from("<H", data, 4)[0] * 0.1
            print(f"  Incline range: {min_inc:.1f} - {max_inc:.1f}% (step: {inc_inc:.1f})")
    except Exception:
        print("  Incline range: not available")


def print_help():
    print("""
  ╔══════════════════════════════════════════════╗
  ║         UREVO CyberPad Controller            ║
  ╠══════════════════════════════════════════════╣
  ║  s <speed>   Set speed (km/h), e.g. s 3.5   ║
  ║  i <incline> Set incline (%), e.g. i 5       ║
  ║  +           Speed up by 0.5 km/h            ║
  ║  -           Speed down by 0.5 km/h          ║
  ║  go          Start / resume                  ║
  ║  stop        Stop                            ║
  ║  pause       Pause                           ║
  ║  info        Show supported ranges           ║
  ║  h           Show this help                  ║
  ║  q           Quit                            ║
  ╚══════════════════════════════════════════════╝
""")


async def dashboard_loop():
    """Periodically refresh the dashboard."""
    while True:
        print_dashboard()
        await asyncio.sleep(1)


async def input_loop(client):
    """Read user commands from stdin."""
    loop = asyncio.get_event_loop()
    while True:
        try:
            cmd = await loop.run_in_executor(None, lambda: input("\n> ").strip().lower())
        except (EOFError, KeyboardInterrupt):
            break

        if not cmd:
            continue
        elif cmd == "q":
            break
        elif cmd == "h":
            print_help()
        elif cmd == "go":
            await start_treadmill(client)
        elif cmd == "stop":
            await stop_treadmill(client)
        elif cmd == "pause":
            await pause_treadmill(client)
        elif cmd == "+":
            new_speed = current_speed + 0.5
            await set_speed(client, new_speed)
        elif cmd == "-":
            new_speed = max(0.5, current_speed - 0.5)
            await set_speed(client, new_speed)
        elif cmd.startswith("s "):
            try:
                speed = float(cmd[2:])
                await set_speed(client, speed)
            except ValueError:
                print("  Usage: s <speed_kmh>  e.g. s 3.5")
        elif cmd.startswith("i "):
            try:
                incline = float(cmd[2:])
                await set_incline(client, incline)
            except ValueError:
                print("  Usage: i <incline_%>  e.g. i 5")
        elif cmd == "info":
            await read_supported_ranges(client)
        else:
            print(f"  Unknown command: {cmd} (type 'h' for help)")

    return "quit"


async def main():
    address = sys.argv[1] if len(sys.argv) > 1 else DEVICE_ADDRESS

    print("╔══════════════════════════════════════════════╗")
    print("║       UREVO CyberPad FTMS Controller        ║")
    print("╚══════════════════════════════════════════════╝")
    print(f"\n  Connecting to {address}...")

    try:
        async with BleakClient(address, timeout=15.0) as client:
            print(f"  Connected: {client.is_connected}\n")

            # Read capabilities
            await read_supported_ranges(client)

            # Request control
            if not await request_control(client):
                print("  WARNING: Could not get control — commands may not work")

            # Subscribe to treadmill data notifications
            try:
                await client.start_notify(TREADMILL_DATA, parse_treadmill_data)
                print("  Subscribed to treadmill data")
            except Exception as e:
                print(f"  Could not subscribe to treadmill data: {e}")

            # Subscribe to machine status
            try:
                await client.start_notify(MACHINE_STATUS, parse_machine_status)
                print("  Subscribed to machine status")
            except Exception as e:
                print(f"  Could not subscribe to machine status: {e}")

            print_help()

            # Run dashboard refresh and input handler concurrently
            dashboard_task = asyncio.create_task(dashboard_loop())
            result = await input_loop(client)
            dashboard_task.cancel()

            if is_running:
                print("\n  Stopping treadmill before disconnect...")
                await stop_treadmill(client)
                await asyncio.sleep(1)

            print("\n  Disconnecting...")

    except Exception as e:
        print(f"\n  Connection failed: {e}")
        print("  Make sure the CyberPad is on and not connected to another app.")
        sys.exit(1)

    print("  Goodbye!")


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        print("\n  Interrupted. Bye!")
