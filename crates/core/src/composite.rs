use crate::tiff_frame::Plane;

#[derive(Clone, Copy, Debug)]
pub struct ChannelRender {
    pub enabled: bool,
    pub color: [u8; 3],
    pub low: f32,
    pub high: f32,
    pub gain: f32,
}

#[derive(Clone, Debug)]
pub struct RgbaFrame {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

pub fn percentile_range(pixels: &[u16], low_pct: f64, high_pct: f64) -> (f32, f32) {
    if pixels.is_empty() {
        return (0.0, 1.0);
    }
    let step = (pixels.len() / 80_000).max(1);
    let mut samples: Vec<u16> = pixels.iter().step_by(step).copied().collect();
    if samples.is_empty() {
        return (0.0, 1.0);
    }
    samples.sort_unstable();
    let at = |pct: f64| -> f32 {
        let pct = pct.clamp(0.0, 100.0);
        let position = (samples.len() - 1) as f64 * pct / 100.0;
        let index = position.round() as usize;
        samples[index.min(samples.len() - 1)] as f32
    };
    let low = at(low_pct);
    let mut high = at(high_pct);
    let peak = samples[samples.len() - 1] as f32;
    // Phase-contrast halos pin the top percentile at the sensor ceiling and
    // crush the rest of the frame. Walk down until the value leaves that rim.
    if peak >= 4096.0 && high >= peak * 0.98 {
        let cutoff = peak * 0.85;
        let mut pct = high_pct;
        while pct > 90.0 {
            pct -= 0.5;
            let candidate = at(pct);
            if candidate < cutoff {
                high = candidate;
                break;
            }
        }
    }
    // Phase contrast keeps a broad bright halo above the cell bodies.
    // Fluorescence keeps a dark field, so its bright tail is the signal.
    let median = at(50.0);
    let p90 = at(90.0);
    let body = p90 - median;
    if median > 2_000.0 && body > 500.0 && high - p90 > body * 3.0 {
        high = p90;
    }
    let high = high.max(low + 1.0);
    (low, high)
}

pub fn composite(planes: &[&Plane], settings: &[ChannelRender]) -> RgbaFrame {
    let Some(first) = planes.first() else {
        return RgbaFrame {
            width: 0,
            height: 0,
            pixels: Vec::new(),
        };
    };
    let width = first.width;
    let height = first.height;
    let count = (width as usize).saturating_mul(height as usize);

    struct Active<'a> {
        pixels: &'a [u16],
        color: [f32; 3],
        low: f32,
        inverse: f32,
    }

    let active: Vec<Active<'_>> = planes
        .iter()
        .zip(settings)
        .filter_map(|(plane, setting)| {
            if !setting.enabled || plane.pixels.len() != count {
                return None;
            }
            let span = (setting.high - setting.low).max(1.0);
            let gain = setting.gain.max(0.0);
            Some(Active {
                pixels: &plane.pixels,
                color: setting.color.map(|channel| channel as f32 / 255.0 * gain),
                low: setting.low,
                inverse: 1.0 / span,
            })
        })
        .collect();

    let mut pixels = vec![0u8; count * 4];
    for index in 0..count {
        let mut red = 0.0;
        let mut green = 0.0;
        let mut blue = 0.0;
        for channel in &active {
            let scaled =
                ((channel.pixels[index] as f32 - channel.low) * channel.inverse).clamp(0.0, 1.0);
            red += scaled * channel.color[0];
            green += scaled * channel.color[1];
            blue += scaled * channel.color[2];
        }
        let offset = index * 4;
        pixels[offset] = (red.clamp(0.0, 1.0) * 255.0) as u8;
        pixels[offset + 1] = (green.clamp(0.0, 1.0) * 255.0) as u8;
        pixels[offset + 2] = (blue.clamp(0.0, 1.0) * 255.0) as u8;
        pixels[offset + 3] = 255;
    }

    RgbaFrame {
        width,
        height,
        pixels,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn plane(values: &[u16]) -> Plane {
        Plane {
            width: values.len() as u32,
            height: 1,
            pixels: Arc::<[u16]>::from(values.to_vec()),
        }
    }

    #[test]
    fn composites_channels_additively_and_clamps() {
        let brightfield = plane(&[0, 1000]);
        let cy3 = plane(&[0, 1000]);
        let frame = composite(
            &[&brightfield, &cy3],
            &[
                ChannelRender {
                    enabled: true,
                    color: [255, 255, 255],
                    low: 0.0,
                    high: 1000.0,
                    gain: 0.5,
                },
                ChannelRender {
                    enabled: true,
                    color: [255, 0, 0],
                    low: 0.0,
                    high: 1000.0,
                    gain: 1.0,
                },
            ],
        );

        assert_eq!(&frame.pixels[0..4], &[0, 0, 0, 255]);
        assert_eq!(frame.pixels[4], 255);
        assert_eq!(frame.pixels[5], 127);
        assert_eq!(frame.pixels[6], 127);
    }

    #[test]
    fn disabled_channel_is_left_out() {
        let cy3 = plane(&[1000]);
        let frame = composite(
            &[&cy3],
            &[ChannelRender {
                enabled: false,
                color: [255, 0, 0],
                low: 0.0,
                high: 1000.0,
                gain: 1.0,
            }],
        );
        assert_eq!(&frame.pixels, &[0, 0, 0, 255]);
    }

    #[test]
    fn percentile_ignores_a_single_hot_pixel() {
        let mut pixels = vec![10u16; 10_000];
        pixels[0] = 60_000;
        let (low, high) = percentile_range(&pixels, 0.1, 99.9);
        assert!(low <= 10.0);
        assert!(high < 1000.0);
    }

    #[test]
    fn percentile_drops_a_saturated_rim() {
        let mut pixels = vec![1_000u16; 9_800];
        pixels.extend(std::iter::repeat_n(65_535, 200));
        let (low, high) = percentile_range(&pixels, 0.1, 99.9);
        assert!(low <= 1_000.0);
        assert!(high < 10_000.0, "high {high}");
    }
}
