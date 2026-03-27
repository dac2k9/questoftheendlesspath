#!/bin/bash
# Wrapper to run the BLE scanner - use this if your terminal lacks BT permission
cd "$(dirname "$0")"
/usr/bin/python3 ble_scan_cyberpad.py "$@" 2>&1
echo ""
echo "Press Enter to close..."
read
