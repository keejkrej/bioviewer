use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use crate::{identify_channel, read_mm_properties, ChannelIdentity};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Axes {
    pub channel: u32,
    pub position: u32,
    pub time: u32,
    pub z: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Axis {
    pub value: u32,
    pub label: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct FrameKey {
    position: u32,
    time: u32,
    z: u32,
    channel: u32,
}

#[derive(Clone, Debug)]
pub struct Series {
    pub root: PathBuf,
    pub positions: Vec<Axis>,
    pub times: Vec<u32>,
    pub zs: Vec<u32>,
    pub channels: Vec<u32>,
    pub parsed_files: usize,
    pub skipped_files: usize,
    frames: HashMap<FrameKey, PathBuf>,
}

#[derive(Clone, Debug)]
pub struct OpenedSeries {
    pub series: Series,
    pub identities: Vec<ChannelIdentity>,
}

pub fn open_series(root: &Path) -> Result<OpenedSeries, String> {
    let series = scan_series(root)?;
    let identities = identify_series_channels(&series);
    Ok(OpenedSeries { series, identities })
}

pub fn parse_mm_filename(name: &str) -> Option<Axes> {
    let channel = number_after_key(name, &["channel"])?;
    let position = number_after_key(name, &["position"]).unwrap_or(0);
    let time = number_after_key(name, &["time"]).unwrap_or(0);
    let z = number_after_key(name, &["_z", "z"]).unwrap_or(0);
    Some(Axes {
        channel,
        position,
        time,
        z,
    })
}

impl Series {
    pub fn frame_path(&self, position: u32, time: u32, z: u32, channel: u32) -> Option<&Path> {
        self.frames
            .get(&FrameKey {
                position,
                time,
                z,
                channel,
            })
            .map(PathBuf::as_path)
    }

    pub fn sample_path(&self, channel: u32) -> Option<&Path> {
        self.frames
            .iter()
            .find(|(key, _)| key.channel == channel)
            .map(|(_, path)| path.as_path())
    }
}

pub fn scan_series(root: &Path) -> Result<Series, String> {
    if !root.is_dir() {
        return Err(format!("{} is not a folder", root.display()));
    }

    let mut hits = Vec::new();
    let mut skipped_files = 0usize;
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) => {
                if directory == root {
                    return Err(format!("failed to list {}: {error}", root.display()));
                }
                continue;
            }
        };
        for entry in entries {
            let Ok(entry) = entry else {
                continue;
            };
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with('.')
                    || name == "$RECYCLE.BIN"
                    || name == "System Volume Information"
                {
                    continue;
                }
                stack.push(path);
                continue;
            }
            if !is_tiff(&path) {
                continue;
            }
            let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                skipped_files += 1;
                continue;
            };
            let Some(axes) = parse_mm_filename(file_name) else {
                skipped_files += 1;
                continue;
            };
            let folder = path
                .parent()
                .filter(|parent| *parent != root)
                .and_then(|parent| parent.file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            hits.push((axes, path, folder));
        }
    }

    if hits.is_empty() {
        return Err(format!(
            "No Micro-Manager TIFFs under {}. Expected names like img_channel000_position001_time000000000_z000.tif. Skipped {skipped_files} other TIFF files.",
            root.display()
        ));
    }

    let mut frames = HashMap::new();
    let mut label_counts: HashMap<u32, HashMap<String, usize>> = HashMap::new();
    let mut times = HashSet::new();
    let mut zs = HashSet::new();
    let mut channels = HashSet::new();
    for (axes, path, folder) in hits {
        times.insert(axes.time);
        zs.insert(axes.z);
        channels.insert(axes.channel);
        if !folder.is_empty() {
            *label_counts
                .entry(axes.position)
                .or_default()
                .entry(folder)
                .or_default() += 1;
        }
        frames.insert(
            FrameKey {
                position: axes.position,
                time: axes.time,
                z: axes.z,
                channel: axes.channel,
            },
            path,
        );
    }

    let mut positions: Vec<Axis> = label_counts
        .keys()
        .copied()
        .chain(
            frames
                .keys()
                .map(|key| key.position)
                .filter(|position| !label_counts.contains_key(position)),
        )
        .collect::<HashSet<_>>()
        .into_iter()
        .map(|value| Axis {
            value,
            label: best_label(value, label_counts.get(&value)),
        })
        .collect();
    positions.sort_by(|left, right| {
        natural_cmp(&left.label, &right.label).then_with(|| left.value.cmp(&right.value))
    });

    let mut times: Vec<u32> = times.into_iter().collect();
    times.sort_unstable();
    let mut zs: Vec<u32> = zs.into_iter().collect();
    zs.sort_unstable();
    let mut channels: Vec<u32> = channels.into_iter().collect();
    channels.sort_unstable();

    Ok(Series {
        root: root.to_path_buf(),
        positions,
        times,
        zs,
        channels,
        parsed_files: frames.len(),
        skipped_files,
        frames,
    })
}

fn identify_series_channels(series: &Series) -> Vec<ChannelIdentity> {
    series
        .channels
        .iter()
        .map(|channel| {
            let properties = series
                .sample_path(*channel)
                .map(read_mm_properties)
                .unwrap_or_default();
            identify_channel(*channel, &properties)
        })
        .collect()
}

fn best_label(value: u32, counts: Option<&HashMap<String, usize>>) -> String {
    counts
        .and_then(|counts| {
            counts
                .iter()
                .max_by_key(|(_, count)| *count)
                .map(|(label, _)| label.clone())
        })
        .filter(|label| !label.is_empty())
        .unwrap_or_else(|| format!("position {value}"))
}

fn is_tiff(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase())
            .as_deref(),
        Some("tif" | "tiff")
    )
}

fn number_after_key(name: &str, keys: &[&str]) -> Option<u32> {
    let lower = name.to_ascii_lowercase();
    keys.iter().find_map(|key| number_after(&lower, key))
}

fn number_after(haystack: &str, key: &str) -> Option<u32> {
    let start = haystack.find(key)? + key.len();
    let rest = haystack[start..].trim_start_matches(['_', '-', ' ']);
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

pub fn natural_cmp(left: &str, right: &str) -> Ordering {
    let mut left_chars = left.chars().peekable();
    let mut right_chars = right.chars().peekable();
    loop {
        match (left_chars.peek().copied(), right_chars.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(left_char), Some(right_char))
                if left_char.is_ascii_digit() && right_char.is_ascii_digit() =>
            {
                let left_number = take_number(&mut left_chars);
                let right_number = take_number(&mut right_chars);
                let ordering = left_number.cmp(&right_number);
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
            (Some(left_char), Some(right_char)) => {
                let ordering = left_char
                    .to_ascii_lowercase()
                    .cmp(&right_char.to_ascii_lowercase());
                if ordering != Ordering::Equal {
                    return ordering;
                }
                left_chars.next();
                right_chars.next();
            }
        }
    }
}

fn take_number(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> u64 {
    let mut number = 0u64;
    while let Some(character) = chars.peek().copied() {
        if !character.is_ascii_digit() {
            break;
        }
        chars.next();
        number = number
            .saturating_mul(10)
            .saturating_add((character as u8 - b'0') as u64);
    }
    number
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_micro_manager_filename() {
        let axes = parse_mm_filename("img_channel001_position012_time000000042_z003.tif").unwrap();
        assert_eq!(
            axes,
            Axes {
                channel: 1,
                position: 12,
                time: 42,
                z: 3,
            }
        );
    }

    #[test]
    fn groups_a_folder_by_filename_axes() {
        let root = std::env::temp_dir().join(format!("bioviewer-series-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("Pos0")).unwrap();
        fs::create_dir_all(root.join("Pos1")).unwrap();
        for (folder, position, channel, time) in [
            ("Pos0", 1, 0, 0),
            ("Pos0", 1, 1, 0),
            ("Pos0", 1, 0, 1),
            ("Pos1", 2, 0, 0),
        ] {
            let name =
                format!("img_channel{channel:03}_position{position:03}_time{time:09}_z000.tif");
            fs::write(root.join(folder).join(name), b"not a tiff").unwrap();
        }
        fs::write(root.join("Pos0").join("notes.tif"), b"skip").unwrap();

        let series = scan_series(&root).unwrap();
        assert_eq!(series.parsed_files, 4);
        assert_eq!(series.skipped_files, 1);
        assert_eq!(series.channels, vec![0, 1]);
        assert_eq!(series.times, vec![0, 1]);
        assert_eq!(series.zs, vec![0]);
        assert_eq!(series.positions.len(), 2);
        assert_eq!(series.positions[0].label, "Pos0");
        assert_eq!(series.positions[1].label, "Pos1");
        assert!(series.frame_path(1, 0, 0, 1).is_some());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn natural_order_puts_pos2_before_pos10() {
        assert_eq!(natural_cmp("Pos2", "Pos10"), Ordering::Less);
        assert_eq!(
            natural_cmp("13-Pos000_001", "13-Pos000_002"),
            Ordering::Less
        );
    }
}
