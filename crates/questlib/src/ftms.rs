/// FTMS Treadmill Data parser.
///
/// Parses the Fitness Machine Service Treadmill Data characteristic (0x2ACD)
/// as defined by the Bluetooth SIG FTMS specification.
///
/// Ported from cyberpad_controller.py.

#[derive(Debug, Clone, Default)]
pub struct TreadmillData {
    pub speed_kmh: f32,
    pub average_speed_kmh: Option<f32>,
    pub total_distance_m: Option<u32>,
    pub incline_pct: Option<f32>,
    pub elapsed_secs: Option<u16>,
}

/// Parse a raw FTMS Treadmill Data notification payload.
///
/// Returns `None` if the data is too short to contain the flags field.
pub fn parse_treadmill_data(data: &[u8]) -> Option<TreadmillData> {
    if data.len() < 2 {
        return None;
    }

    let flags = u16::from_le_bytes([data[0], data[1]]);
    let mut offset = 2;
    let mut result = TreadmillData::default();

    // Bit 0: More Data — if 0, Instantaneous Speed is present
    if flags & 0x0001 == 0 {
        if offset + 2 > data.len() {
            return Some(result);
        }
        let raw = u16::from_le_bytes([data[offset], data[offset + 1]]);
        result.speed_kmh = f32::from(raw) * 0.01;
        offset += 2;
    }

    // Bit 1: Average Speed present
    if flags & 0x0002 != 0 {
        if offset + 2 <= data.len() {
            let raw = u16::from_le_bytes([data[offset], data[offset + 1]]);
            result.average_speed_kmh = Some(f32::from(raw) * 0.01);
        }
        offset += 2;
    }

    // Bit 2: Total Distance present (24-bit unsigned LE)
    if flags & 0x0004 != 0 {
        if offset + 3 <= data.len() {
            let raw = u32::from_le_bytes([data[offset], data[offset + 1], data[offset + 2], 0]);
            result.total_distance_m = Some(raw);
        }
        offset += 3;
    }

    // Bit 3: Inclination and Ramp Angle present
    if flags & 0x0008 != 0 {
        if offset + 2 <= data.len() {
            let raw = i16::from_le_bytes([data[offset], data[offset + 1]]);
            result.incline_pct = Some(f32::from(raw) * 0.1);
        }
        offset += 4; // incline (2) + ramp angle (2)
    }

    // Bit 4: Elevation Gain present
    if flags & 0x0010 != 0 {
        offset += 4;
    }

    // Bit 5: Instantaneous Pace present
    if flags & 0x0020 != 0 {
        offset += 1;
    }

    // Bit 6: Average Pace present
    if flags & 0x0040 != 0 {
        offset += 1;
    }

    // Bit 7: Expended Energy present
    if flags & 0x0080 != 0 {
        offset += 5; // total(2) + per_hour(2) + per_min(1)
    }

    // Bit 8: Heart Rate present
    if flags & 0x0100 != 0 {
        offset += 1;
    }

    // Bit 9: Metabolic Equivalent present
    if flags & 0x0200 != 0 {
        offset += 1;
    }

    // Bit 10: Elapsed Time present
    if flags & 0x0400 != 0 {
        if offset + 2 <= data.len() {
            let raw = u16::from_le_bytes([data[offset], data[offset + 1]]);
            result.elapsed_secs = Some(raw);
        }
        // offset += 2;
    }

    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_speed_only() {
        // Flags: 0x0000 (speed present), speed = 350 (3.50 km/h)
        let data = [0x00, 0x00, 0x5E, 0x01];
        let result = parse_treadmill_data(&data).unwrap();
        assert!((result.speed_kmh - 3.50).abs() < 0.01);
    }

    #[test]
    fn parse_speed_and_distance() {
        // Flags: 0x0004 (speed present + distance present)
        // Speed: 500 (5.00 km/h), Distance: 1500m (24-bit LE)
        let data = [0x04, 0x00, 0xF4, 0x01, 0xDC, 0x05, 0x00];
        let result = parse_treadmill_data(&data).unwrap();
        assert!((result.speed_kmh - 5.00).abs() < 0.01);
        assert_eq!(result.total_distance_m, Some(1500));
    }

    #[test]
    fn parse_with_incline() {
        // Flags: 0x000C (speed present + distance + incline)
        // Speed: 400, Distance: 1000, Incline: 50 (5.0%), Ramp: 0
        let data = [0x0C, 0x00, 0x90, 0x01, 0xE8, 0x03, 0x00, 0x32, 0x00, 0x00, 0x00];
        let result = parse_treadmill_data(&data).unwrap();
        assert!((result.speed_kmh - 4.00).abs() < 0.01);
        assert_eq!(result.total_distance_m, Some(1000));
        assert!((result.incline_pct.unwrap() - 5.0).abs() < 0.1);
    }

    #[test]
    fn too_short() {
        assert!(parse_treadmill_data(&[0x00]).is_none());
    }
}
