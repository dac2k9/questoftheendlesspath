#!/usr/bin/env python3
"""
BLE Scanner for UREVO CyberPad
Scans for nearby BLE devices, identifies likely UREVO/treadmill devices,
and enumerates their services and characteristics.
"""

import asyncio
import sys
import platform
import logging
import signal
import faulthandler

# Enable faulthandler to get a traceback on abort/segfault
faulthandler.enable()

# Set up logging
logging.basicConfig(
    level=logging.DEBUG,
    format="%(asctime)s [%(levelname)s] %(name)s: %(message)s",
)
log = logging.getLogger("cyberpad-scan")

def handle_signal(signum, frame):
    log.error(f"Received signal {signum} ({signal.Signals(signum).name})")
    sys.exit(1)

signal.signal(signal.SIGABRT, handle_signal)

log.info(f"Python {sys.version}")
log.info(f"Platform: {platform.platform()}")
log.info(f"macOS version: {platform.mac_ver()[0]}")

log.info("Importing bleak...")
try:
    import bleak
    from bleak import BleakScanner, BleakClient
    log.info(f"bleak imported successfully from {bleak.__file__}")
except Exception as e:
    log.error(f"Failed to import bleak: {e}")
    sys.exit(1)

log.info("Checking CoreBluetooth availability...")
try:
    import CoreBluetooth
    log.info("CoreBluetooth imported OK")
except Exception as e:
    log.warning(f"CoreBluetooth import failed: {e}")

try:
    import objc
    log.info(f"pyobjc available: {objc.__file__}")
except Exception as e:
    log.warning(f"pyobjc not available: {e}")

# Well-known FTMS UUIDs
FTMS_SERVICE = "00001826-0000-1000-8000-00805f9b34fb"
FTMS_UUIDS = {
    "00002acc-0000-1000-8000-00805f9b34fb": "Fitness Machine Feature",
    "00002acd-0000-1000-8000-00805f9b34fb": "Treadmill Data",
    "00002ace-0000-1000-8000-00805f9b34fb": "Cross Trainer Data",
    "00002ad0-0000-1000-8000-00805f9b34fb": "Supported Speed Range",
    "00002ad1-0000-1000-8000-00805f9b34fb": "Supported Inclination Range",
    "00002ad2-0000-1000-8000-00805f9b34fb": "Supported Resistance Level Range",
    "00002ad3-0000-1000-8000-00805f9b34fb": "Supported Power Range",
    "00002ad4-0000-1000-8000-00805f9b34fb": "Supported Heart Rate Range",
    "00002ad6-0000-1000-8000-00805f9b34fb": "Fitness Machine Status",
    "00002ad8-0000-1000-8000-00805f9b34fb": "Training Status",
    "00002ad9-0000-1000-8000-00805f9b34fb": "Fitness Machine Control Point",
}

KNOWN_SERVICES = {
    "00001800-0000-1000-8000-00805f9b34fb": "Generic Access",
    "00001801-0000-1000-8000-00805f9b34fb": "Generic Attribute",
    "0000180a-0000-1000-8000-00805f9b34fb": "Device Information",
    "0000180d-0000-1000-8000-00805f9b34fb": "Heart Rate",
    "0000180f-0000-1000-8000-00805f9b34fb": "Battery Service",
    "00001816-0000-1000-8000-00805f9b34fb": "Cycling Speed and Cadence",
    FTMS_SERVICE: "** Fitness Machine Service (FTMS) **",
}

UREVO_KEYWORDS = ["urevo", "cyber", "treadmill", "walking", "pad", "urtm"]


def is_likely_urevo(name: str) -> bool:
    if not name:
        return False
    lower = name.lower()
    return any(kw in lower for kw in UREVO_KEYWORDS)


async def scan_devices(duration: float = 10.0):
    log.info(f"Starting BLE scan for {duration}s...")
    print(f"Scanning for BLE devices for {duration}s...")
    print("Make sure your CyberPad is ON and not connected to another app.\n")

    log.info("Creating BleakScanner...")
    try:
        scanner = BleakScanner()
        log.info(f"Scanner created: {scanner}")
    except Exception as e:
        log.error(f"Failed to create scanner: {e}", exc_info=True)
        return []

    log.info("Starting scanner.start()...")
    try:
        await scanner.start()
        log.info("Scanner started successfully, waiting...")
    except Exception as e:
        log.error(f"Scanner.start() failed: {e}", exc_info=True)
        return []

    await asyncio.sleep(duration)

    log.info("Stopping scanner...")
    await scanner.stop()

    devices = scanner.discovered_devices_and_advertisement_data
    log.info(f"Scan complete. Found {len(devices)} devices.")

    if not devices:
        print("No BLE devices found. Check Bluetooth is enabled.")
        return []

    # Sort: likely UREVO first, then by signal strength
    sorted_devices = sorted(
        devices.items(),
        key=lambda x: (not is_likely_urevo(x[1][1].local_name or x[1][0].name or ""), -(x[1][1].rssi or -100)),
    )

    print(f"Found {len(sorted_devices)} BLE devices:\n")
    print(f"{'NAME':<30} {'ADDRESS':<40} {'RSSI':>5}  SERVICES")
    print("-" * 110)

    candidates = []
    for address, (device, adv_data) in sorted_devices:
        name = adv_data.local_name or device.name or "Unknown"
        rssi = adv_data.rssi or 0
        service_uuids = adv_data.service_uuids or []

        is_urevo = is_likely_urevo(name)
        has_ftms = FTMS_SERVICE in [u.lower() for u in service_uuids]

        marker = ""
        if is_urevo:
            marker = " <-- LIKELY UREVO"
        if has_ftms:
            marker += " [FTMS!]"

        svc_summary = []
        for uuid in service_uuids:
            known = KNOWN_SERVICES.get(uuid.lower(), "")
            svc_summary.append(known if known else uuid[:8] + "...")

        print(f"{name:<30} {address:<40} {rssi:>5}  {', '.join(svc_summary)}{marker}")

        if is_urevo or has_ftms:
            candidates.append((address, device, adv_data))

    return candidates


async def inspect_device(address: str):
    log.info(f"Connecting to {address}...")
    print(f"\nConnecting to {address}...")
    try:
        async with BleakClient(address, timeout=15.0) as client:
            log.info(f"Connected: {client.is_connected}")
            print(f"Connected: {client.is_connected}")
            print(f"\nServices and Characteristics:")
            print("=" * 80)

            for service in client.services:
                svc_name = KNOWN_SERVICES.get(service.uuid.lower(), service.description or "Unknown Service")
                is_ftms = service.uuid.lower() == FTMS_SERVICE
                prefix = ">>>" if is_ftms else "   "
                print(f"\n{prefix} Service: {svc_name}")
                print(f"{prefix}   UUID: {service.uuid}")

                for char in service.characteristics:
                    char_name = FTMS_UUIDS.get(char.uuid.lower(), char.description or "Unknown")
                    props = ", ".join(char.properties)
                    print(f"      Characteristic: {char_name}")
                    print(f"        UUID: {char.uuid}")
                    print(f"        Properties: {props}")

                    if "read" in char.properties:
                        try:
                            value = await client.read_gatt_char(char.uuid)
                            print(f"        Value: {value.hex()} ({value})")
                        except Exception as e:
                            print(f"        Value: <read error: {e}>")

                    for desc in char.descriptors:
                        print(f"        Descriptor: {desc.uuid} - {desc.description}")

            print("\n" + "=" * 80)
            print("Done inspecting device.")

    except Exception as e:
        log.error(f"Failed to connect: {e}", exc_info=True)
        print(f"Failed to connect: {e}")


async def main():
    log.info("=== CyberPad BLE Scanner starting ===")
    candidates = await scan_devices(duration=10.0)

    if not candidates:
        print("\nNo UREVO/FTMS devices found automatically.")
        print("If your CyberPad is on, it might use a different name.")
        print("Look through the device list above and note the address.")
        print(f"\nTo manually inspect a device, run:")
        print(f"  python3 {sys.argv[0]} <ADDRESS>")
        return

    print(f"\n{'=' * 80}")
    print(f"Found {len(candidates)} candidate device(s). Inspecting...\n")

    for address, device, adv_data in candidates:
        name = adv_data.local_name or device.name or "Unknown"
        print(f"\n--- Inspecting: {name} ({address}) ---")
        await inspect_device(address)


async def main_with_address(address: str):
    log.info(f"Directly inspecting device at {address}")
    await inspect_device(address)


if __name__ == "__main__":
    log.info("Script starting...")
    try:
        if len(sys.argv) > 1:
            asyncio.run(main_with_address(sys.argv[1]))
        else:
            asyncio.run(main())
    except Exception as e:
        log.error(f"Unhandled exception: {e}", exc_info=True)
    log.info("Script exiting.")
