use std::{fs::File, io::BufReader, path::Path, sync::Arc};

use tiff::decoder::{Decoder, DecodingResult};

use crate::Series;

#[derive(Clone, Debug)]
pub struct Plane {
    pub width: u32,
    pub height: u32,
    pub pixels: Arc<[u16]>,
}

#[derive(Clone, Debug)]
pub struct LoadedChannel {
    pub index: u32,
    pub plane: Plane,
}

#[derive(Clone, Debug)]
pub struct Moment {
    pub width: u32,
    pub height: u32,
    pub channels: Vec<LoadedChannel>,
}

pub fn load_tiff_plane(path: &Path) -> Result<Plane, String> {
    let file = File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut decoder = Decoder::new(BufReader::with_capacity(1 << 20, file))
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let (width, height) = decoder
        .dimensions()
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let data = decoder
        .read_image()
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let pixels = to_u16(width, height, data)?;
    Ok(Plane {
        width,
        height,
        pixels: Arc::from(pixels),
    })
}

pub fn load_moment(series: &Series, position: u32, time: u32, z: u32) -> Result<Moment, String> {
    let mut channels = Vec::new();
    let mut width = 0;
    let mut height = 0;
    for channel in &series.channels {
        let Some(path) = series.frame_path(position, time, z, *channel) else {
            continue;
        };
        let plane = load_tiff_plane(path)?;
        if width == 0 {
            width = plane.width;
            height = plane.height;
        } else if plane.width != width || plane.height != height {
            return Err(format!(
                "channel {channel} is {}x{}, expected {width}x{height}",
                plane.width, plane.height
            ));
        }
        channels.push(LoadedChannel {
            index: *channel,
            plane,
        });
    }
    if channels.is_empty() {
        return Err("no channel files for this position, time, and z".to_string());
    }
    Ok(Moment {
        width,
        height,
        channels,
    })
}

fn to_u16(width: u32, height: u32, data: DecodingResult) -> Result<Vec<u16>, String> {
    let expected = (width as usize).saturating_mul(height as usize);
    let values = match data {
        DecodingResult::U8(values) => values.into_iter().map(u16::from).collect(),
        DecodingResult::U16(values) => values,
        _ => return Err("TIFF pixel type is not 8-bit or 16-bit grayscale".to_string()),
    };
    if values.len() != expected {
        return Err(format!(
            "TIFF has {} samples for a {width}x{height} frame",
            values.len()
        ));
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf, time::Instant};

    use super::*;
    use crate::{composite, open_series, percentile_range, ChannelRender};

    #[test]
    fn composites_series_from_env() {
        let Some(root) = std::env::var_os("BIOVIEWER_SERIES").map(PathBuf::from) else {
            eprintln!("skipping real frame test, BIOVIEWER_SERIES is unset");
            return;
        };
        if !root.is_dir() {
            eprintln!("skipping real frame test, {root:?} is absent");
            return;
        }

        let started = Instant::now();
        let opened = open_series(&root).expect("scan");
        let scan_elapsed = started.elapsed();
        assert_eq!(opened.series.channels, vec![0, 1]);
        assert!(opened.series.times.len() > 100);
        assert_eq!(opened.identities[0].name, "Brightfield");
        assert_eq!(opened.identities[1].name, "Cy3");

        let time = opened.series.times[opened.series.times.len() / 2];
        let position = opened.series.positions[0].value;
        let z = opened.series.zs[0];
        let load_started = Instant::now();
        let moment = load_moment(&opened.series, position, time, z).expect("load");
        let load_elapsed = load_started.elapsed();
        assert_eq!(moment.width, 2048);
        assert_eq!(moment.height, 2048);
        assert_eq!(moment.channels.len(), 2);

        let mut settings = Vec::new();
        for (identity, loaded) in opened.identities.iter().zip(&moment.channels) {
            let (low, high) = percentile_range(&loaded.plane.pixels, 0.1, 99.9);
            settings.push(ChannelRender {
                enabled: true,
                color: identity.color,
                low,
                high,
                gain: identity.gain,
            });
            eprintln!(
                "channel {} {} low={low} high={high} gain={} detail={}",
                identity.index, identity.name, identity.gain, identity.detail
            );
        }

        let planes: Vec<&crate::Plane> = moment
            .channels
            .iter()
            .map(|channel| &channel.plane)
            .collect();
        let both = composite(&planes, &settings);
        let mut brightfield_only = settings.clone();
        brightfield_only[1].enabled = false;
        let brightfield = composite(&planes, &brightfield_only);
        let mut cy3_only = settings.clone();
        cy3_only[0].enabled = false;
        let cy3 = composite(&planes, &cy3_only);

        let cy3_max = channel_max(&cy3);
        assert!(cy3_max[0] > 200, "cy3 red {}", cy3_max[0]);
        assert!(cy3_max[2] < 40, "cy3 blue {}", cy3_max[2]);
        let brightfield_max = channel_max(&brightfield);
        // Brightfield gain is below 1, so a clipped pixel stays gray.
        assert!(
            brightfield_max[0] > 80,
            "brightfield red {}",
            brightfield_max[0]
        );
        assert!((brightfield_max[0] as i16 - brightfield_max[1] as i16).abs() < 5);
        assert!((brightfield_max[1] as i16 - brightfield_max[2] as i16).abs() < 5);

        let preview = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/pi-cy3-preview");
        fs::create_dir_all(&preview).unwrap();
        write_thumbnail(&both, &preview.join("composite.png"), 640).unwrap();
        write_thumbnail(&cy3, &preview.join("cy3.png"), 640).unwrap();
        write_thumbnail(&brightfield, &preview.join("brightfield.png"), 640).unwrap();
        eprintln!(
            "scan {scan_elapsed:?} load+decode {load_elapsed:?} time={time} wrote {}",
            preview.display()
        );
    }

    fn channel_max(frame: &crate::RgbaFrame) -> [u8; 3] {
        let mut max = [0u8; 3];
        for pixel in frame.pixels.as_chunks::<4>().0 {
            max[0] = max[0].max(pixel[0]);
            max[1] = max[1].max(pixel[1]);
            max[2] = max[2].max(pixel[2]);
        }
        max
    }

    fn write_thumbnail(
        frame: &crate::RgbaFrame,
        path: &std::path::Path,
        max_edge: u32,
    ) -> Result<(), String> {
        let scale = (max_edge as f32 / frame.width.max(frame.height) as f32).min(1.0);
        let width = ((frame.width as f32 * scale).round() as u32).max(1);
        let height = ((frame.height as f32 * scale).round() as u32).max(1);
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        for y in 0..height {
            let source_y = (y as u64 * frame.height as u64 / height as u64) as u32;
            for x in 0..width {
                let source_x = (x as u64 * frame.width as u64 / width as u64) as u32;
                let source = ((source_y as usize * frame.width as usize) + source_x as usize) * 4;
                let destination = ((y as usize * width as usize) + x as usize) * 4;
                pixels[destination..destination + 4]
                    .copy_from_slice(&frame.pixels[source..source + 4]);
            }
        }
        let file = fs::File::create(path).map_err(|error| error.to_string())?;
        let mut encoder = png::Encoder::new(file, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|error| error.to_string())?;
        writer
            .write_image_data(&pixels)
            .map_err(|error| error.to_string())?;
        Ok(())
    }
}
