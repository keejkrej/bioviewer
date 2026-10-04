use std::{collections::HashMap, fs::File, io::Read, path::Path};

#[derive(Clone, Debug, PartialEq)]
pub struct ChannelIdentity {
    pub index: u32,
    pub name: String,
    pub detail: String,
    pub color: [u8; 3],
    pub gain: f32,
}

pub fn read_mm_properties(path: &Path) -> HashMap<String, String> {
    let Ok(mut file) = File::open(path) else {
        return HashMap::new();
    };
    let mut buffer = vec![0u8; 1024 * 1024];
    let Ok(size) = file.read(&mut buffer) else {
        return HashMap::new();
    };
    buffer.truncate(size);
    parse_mm_properties(&buffer)
}

pub fn parse_mm_properties(buffer: &[u8]) -> HashMap<String, String> {
    let needle = utf16_le("\"Width\"");
    let Some(hit) = find_slice(buffer, &needle) else {
        return HashMap::new();
    };
    let mut start = hit;
    while start >= 2 {
        if buffer[start] == b'{' && buffer.get(start + 1) == Some(&0) {
            break;
        }
        start -= 2;
    }
    if buffer.get(start) != Some(&b'{') {
        return HashMap::new();
    }

    let mut units = Vec::new();
    let mut cursor = start;
    while cursor + 1 < buffer.len() && units.len() < 500_000 {
        let unit = u16::from_le_bytes([buffer[cursor], buffer[cursor + 1]]);
        if unit == 0 {
            break;
        }
        units.push(unit);
        cursor += 2;
    }
    scan_string_pairs(&String::from_utf16_lossy(&units))
}

pub fn identify_channel(index: u32, properties: &HashMap<String, String>) -> ChannelIdentity {
    let filter = [
        "TIFilterBlock1-Label",
        "ZeissReflectorTurret-Label",
        "Channel",
    ]
    .iter()
    .find_map(|key| properties.get(*key))
    .cloned()
    .unwrap_or_default();
    let exposure = properties
        .get("HamamatsuHam_DCAM-Exposure")
        .or_else(|| properties.get("Exposure-ms"))
        .cloned()
        .unwrap_or_default();
    let (name, color, gain) = classify(index, &filter);
    let mut detail = Vec::new();
    if !filter.is_empty() {
        detail.push(format!("filter {filter}"));
    }
    if !exposure.is_empty() {
        detail.push(format!("{exposure} ms"));
    }
    detail.push(format!("channel {index:03}"));
    ChannelIdentity {
        index,
        name,
        detail: detail.join(" · "),
        color,
        gain,
    }
}

fn classify(index: u32, filter: &str) -> (String, [u8; 3], f32) {
    let normalized = filter.to_ascii_lowercase();
    let compact: String = normalized
        .chars()
        .filter(|character| !character.is_ascii_whitespace() && *character != '-')
        .collect();
    if normalized.contains("rfp")
        || normalized.contains("cy3")
        || normalized.contains("tritc")
        || normalized.contains("mcherry")
        || normalized.contains("texred")
        || normalized.contains("texas")
        || normalized.contains("555")
        || normalized.contains("594")
        || normalized.contains("568")
        || normalized.contains("625")
    {
        return ("Cy3".to_string(), [255, 96, 16], 1.0);
    }
    if normalized.contains("fitc") || normalized.contains("gfp") || normalized.contains("488") {
        return ("FITC".to_string(), [40, 220, 60], 1.0);
    }
    if normalized.contains("dapi") || normalized.contains("hoechst") || normalized.contains("405") {
        return ("DAPI".to_string(), [70, 120, 255], 1.0);
    }
    if normalized.contains("cy5") || normalized.contains("647") || normalized.contains("far red") {
        return ("Cy5".to_string(), [230, 0, 190], 1.0);
    }
    if filter.trim().is_empty() {
        return (format!("channel {index:03}"), palette(index), 1.0);
    }
    if compact.contains("none")
        || normalized.contains("----")
        || normalized.contains("durchlicht")
        || normalized.contains("bright")
        || normalized.contains("phase")
        || normalized.contains("dic")
    {
        return ("Brightfield".to_string(), [255, 255, 255], 0.6);
    }
    (filter.to_string(), palette(index), 1.0)
}

fn palette(index: u32) -> [u8; 3] {
    const COLORS: [[u8; 3]; 6] = [
        [255, 255, 255],
        [40, 220, 60],
        [255, 0, 180],
        [255, 140, 0],
        [0, 220, 220],
        [255, 60, 60],
    ];
    COLORS[(index as usize) % COLORS.len()]
}

fn utf16_le(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .flat_map(|unit| unit.to_le_bytes())
        .collect()
}

fn find_slice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn scan_string_pairs(text: &str) -> HashMap<String, String> {
    let bytes = text.as_bytes();
    let mut pairs = HashMap::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] != b'"' {
            cursor += 1;
            continue;
        }
        let Some((key, after_key)) = parse_json_string(text, cursor) else {
            cursor += 1;
            continue;
        };
        let mut value_at = after_key;
        while value_at < bytes.len() && bytes[value_at].is_ascii_whitespace() {
            value_at += 1;
        }
        if bytes.get(value_at) != Some(&b':') {
            cursor = after_key;
            continue;
        }
        value_at += 1;
        while value_at < bytes.len() && bytes[value_at].is_ascii_whitespace() {
            value_at += 1;
        }
        if bytes.get(value_at) != Some(&b'"') {
            cursor = value_at;
            continue;
        }
        let Some((value, after_value)) = parse_json_string(text, value_at) else {
            cursor = value_at + 1;
            continue;
        };
        pairs.insert(key, value);
        cursor = after_value;
    }
    pairs
}

fn parse_json_string(text: &str, start: usize) -> Option<(String, usize)> {
    let bytes = text.as_bytes();
    if bytes.get(start) != Some(&b'"') {
        return None;
    }
    let mut value = String::new();
    let mut cursor = start + 1;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'\\' => {
                let escaped = *bytes.get(cursor + 1)?;
                value.push(escaped as char);
                cursor += 2;
            }
            b'"' => return Some((value, cursor + 1)),
            byte => {
                value.push(byte as char);
                cursor += 1;
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn embed(json: &str) -> Vec<u8> {
        let mut bytes = vec![0, 0, 9, 0];
        bytes.extend(utf16_le(json));
        bytes.extend_from_slice(&[0, 0]);
        bytes
    }

    #[test]
    fn reads_utf16_filter_label() {
        let buffer = embed(
            r#"{ "Width": 2048, "TIFilterBlock1-Label": "2-RFP", "HamamatsuHam_DCAM-Exposure": "250.00" }"#,
        );
        let properties = parse_mm_properties(&buffer);
        let identity = identify_channel(1, &properties);
        assert_eq!(identity.name, "Cy3");
        assert_eq!(identity.color, [255, 96, 16]);
        assert!(identity.detail.contains("2-RFP"));
        assert!(identity.detail.contains("250.00"));
    }

    #[test]
    fn empty_nikon_cube_is_brightfield() {
        let buffer = embed(r#"{ "Width": 8, "TIFilterBlock1-Label": "1------" }"#);
        let identity = identify_channel(0, &parse_mm_properties(&buffer));
        assert_eq!(identity.name, "Brightfield");
        assert!(identity.gain < 1.0);
    }

    #[test]
    fn zeiss_empty_reflector_is_brightfield() {
        let buffer = embed(r#"{ "Width": 8, "ZeissReflectorTurret-Label": "2- none" }"#);
        let identity = identify_channel(0, &parse_mm_properties(&buffer));
        assert_eq!(identity.name, "Brightfield");
    }
}
