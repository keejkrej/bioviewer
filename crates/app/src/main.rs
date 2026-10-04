use std::{fs, path::PathBuf, sync::Arc, time::Duration};

use bioviewer_core::{
    composite, load_moment, natural_cmp, open_series, percentile_range, ChannelIdentity,
    ChannelRender, Moment, OpenedSeries, Plane, RgbaFrame, Series,
};
use gpui_kit::component::{
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    scroll::ScrollableElement,
    slider::{Slider, SliderEvent, SliderScale, SliderState},
};
use gpui_kit::{
    actions, div, img, px, rgb, size, AppContext as _, Bounds, Context, Entity, FocusHandle,
    ImageSource, InteractiveElement, IntoElement, KeyBinding, ObjectFit, ParentElement, Render,
    RenderImage, SharedString, Styled, StyledImage, Subscription, Task, TitlebarOptions, Window,
    WindowBounds, WindowOptions,
};

actions!(
    bioviewer,
    [NextTime, PrevTime, NextPosition, PrevPosition, TogglePlay]
);

const PRESETS: &[(&str, &str, [u8; 3], f32)] = &[
    ("BF", "Brightfield", [255, 255, 255], 0.6),
    ("DAPI", "DAPI", [70, 120, 255], 1.0),
    ("FITC", "FITC", [40, 220, 60], 1.0),
    ("Cy3", "Cy3", [255, 96, 16], 1.0),
    ("Cy5", "Cy5", [230, 0, 190], 1.0),
];

struct ChannelControl {
    index: u32,
    name: String,
    detail: String,
    enabled: bool,
    color: [u8; 3],
    low: f32,
    high: f32,
    gain: f32,
    auto: bool,
    window_slider: Entity<SliderState>,
    brightness_slider: Entity<SliderState>,
}

impl ChannelControl {
    fn from_identity(identity: ChannelIdentity, cx: &mut Context<BioViewer>) -> Self {
        let gain = identity.gain.clamp(0.0, 3.0);
        let window_slider = cx.new(|_| {
            SliderState::new()
                .min(1.0)
                .max(65535.0)
                .step(1.0)
                .scale(SliderScale::Logarithmic)
                .default_value((1.0, 65535.0))
        });
        let brightness_slider = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(3.0)
                .step(0.01)
                .default_value(gain)
        });
        Self {
            index: identity.index,
            name: identity.name,
            detail: identity.detail,
            enabled: true,
            color: identity.color,
            low: 1.0,
            high: 65535.0,
            gain,
            auto: true,
            window_slider,
            brightness_slider,
        }
    }

    fn render_settings(&self) -> ChannelRender {
        ChannelRender {
            enabled: self.enabled,
            color: self.color,
            low: self.low,
            high: self.high.max(self.low + 1.0),
            gain: self.gain,
        }
    }
}

struct Listed {
    name: String,
    path: PathBuf,
    is_dir: bool,
}

enum Left {
    Browser,
    Positions,
}

struct FrameRequest {
    series: Arc<Series>,
    position: u32,
    time: u32,
    z: u32,
}

impl FrameRequest {
    fn execute(self) -> Result<Moment, String> {
        load_moment(&self.series, self.position, self.time, self.z)
    }
}

struct BioViewer {
    focus: FocusHandle,
    browse: PathBuf,
    entries: Vec<Listed>,
    has_display_settings: bool,
    left: Left,
    series: Option<Arc<Series>>,
    channels: Vec<ChannelControl>,
    position_index: usize,
    time_index: usize,
    z_index: usize,
    moment: Option<Moment>,
    rendered: Option<Arc<RenderImage>>,
    frame_width: u32,
    frame_height: u32,
    frame_epoch: u64,
    scale: Option<f32>,
    playing: bool,
    loading: bool,
    status: String,
    revision: u64,
    time_slider: Entity<SliderState>,
    _time_sub: Subscription,
    channel_subs: Vec<Subscription>,
    load_task: Option<Task<()>>,
    play_task: Option<Task<()>>,
    scan_task: Option<Task<()>>,
}

impl BioViewer {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let browse = default_root();
        let time_slider = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(0.0)
                .step(1.0)
                .default_value(0.0)
        });
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let _time_sub = cx.subscribe_in(&time_slider, window, |this, _, event, window, cx| {
            if let SliderEvent::Change(value) = event {
                let index = value.start().round().max(0.0) as usize;
                this.on_time_slider(index, window, cx);
            }
        });
        let mut viewer = Self {
            focus,
            browse,
            entries: Vec::new(),
            has_display_settings: false,
            left: Left::Browser,
            series: None,
            channels: Vec::new(),
            position_index: 0,
            time_index: 0,
            z_index: 0,
            moment: None,
            rendered: None,
            frame_width: 0,
            frame_height: 0,
            frame_epoch: 0,
            scale: None,
            playing: false,
            loading: false,
            status: "Open a Micro-Manager folder. Filenames supply position, channel, time, and z."
                .to_string(),
            revision: 0,
            time_slider,
            _time_sub,
            channel_subs: Vec::new(),
            load_task: None,
            play_task: None,
            scan_task: None,
        };
        viewer.refresh_listing();
        viewer
    }

    fn refresh_listing(&mut self) {
        let mut entries = Vec::new();
        if let Ok(read) = fs::read_dir(&self.browse) {
            for entry in read.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('.')
                    || name == "$RECYCLE.BIN"
                    || name == "System Volume Information"
                {
                    continue;
                }
                let path = entry.path();
                let is_dir = entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
                if is_dir || is_image_file(&path) {
                    entries.push(Listed { name, path, is_dir });
                }
            }
        }
        entries.sort_by(|left, right| {
            right
                .is_dir
                .cmp(&left.is_dir)
                .then_with(|| natural_cmp(&left.name, &right.name))
        });
        self.has_display_settings = self.browse.join("DisplaySettings.json").is_file();
        self.entries = entries;
    }

    fn go_up(&mut self, cx: &mut Context<Self>) {
        if let Some(parent) = self.browse.parent() {
            if parent != self.browse {
                self.browse = parent.to_path_buf();
                self.refresh_listing();
                cx.notify();
            }
        }
    }

    fn choose_folder(&mut self, cx: &mut Context<Self>) {
        let mut dialog = rfd::FileDialog::new();
        if self.browse.is_dir() {
            dialog = dialog.set_directory(&self.browse);
        }
        if let Some(path) = dialog.pick_folder() {
            self.browse = path;
            self.left = Left::Browser;
            self.refresh_listing();
            cx.notify();
        }
    }

    fn activate_entry(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.entries.get(index) else {
            return;
        };
        if entry.is_dir {
            self.browse = entry.path.clone();
            self.refresh_listing();
            cx.notify();
            return;
        }
        let extension = entry
            .path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase())
            .unwrap_or_default();
        if extension == "tif" || extension == "tiff" {
            if let Some(parent) = entry.path.parent() {
                self.browse = parent.to_path_buf();
                self.refresh_listing();
                self.begin_scan(window, cx);
            }
            return;
        }
        self.status = format!(
            "{} is a single file. This build opens a folder of Micro-Manager TIFFs (img_channel…_position…_time…_z…).",
            entry.name
        );
        cx.notify();
    }

    fn begin_scan(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let root = self.browse.clone();
        self.playing = false;
        self.play_task = None;
        self.status = format!("Scanning {}", root.display());
        cx.notify();
        self.scan_task = Some(cx.spawn_in(window, async move |this, cx| {
            let started = std::time::Instant::now();
            let result = cx.background_spawn(async move { open_series(&root) }).await;
            let elapsed = started.elapsed();
            this.update_in(cx, |view, window, cx| match result {
                Ok(opened) => view.install(opened, elapsed, window, cx),
                Err(error) => {
                    view.status = error;
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    fn install(
        &mut self,
        opened: OpenedSeries,
        elapsed: std::time::Duration,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let series = Arc::new(opened.series);
        let max = series.times.len().saturating_sub(1) as f32;
        self.time_slider = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(max)
                .step(1.0)
                .default_value(0.0)
        });
        self.subscribe_time(window, cx);
        self.channel_subs.clear();
        self.channels = opened
            .identities
            .into_iter()
            .map(|identity| ChannelControl::from_identity(identity, cx))
            .collect();
        self.subscribe_channels(window, cx);
        self.status = format!(
            "{} · {} positions · {} times · {} z · {} channels · {} files · {:.1}s",
            series.root.display(),
            series.positions.len(),
            series.times.len(),
            series.zs.len(),
            series.channels.len(),
            series.parsed_files,
            elapsed.as_secs_f32()
        );
        self.series = Some(series);
        self.position_index = 0;
        self.time_index = 0;
        self.z_index = 0;
        self.moment = None;
        self.rendered = None;
        self.left = Left::Positions;
        self.load_now(window, cx);
    }

    fn subscribe_time(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self._time_sub =
            cx.subscribe_in(&self.time_slider, window, |this, _, event, window, cx| {
                if let SliderEvent::Change(value) = event {
                    let index = value.start().round().max(0.0) as usize;
                    this.on_time_slider(index, window, cx);
                }
            });
    }

    fn subscribe_channels(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let sliders: Vec<_> = self
            .channels
            .iter()
            .map(|channel| {
                (
                    channel.index,
                    channel.window_slider.clone(),
                    channel.brightness_slider.clone(),
                )
            })
            .collect();
        self.channel_subs.clear();
        for (index, window_slider, brightness_slider) in sliders {
            self.channel_subs.push(cx.subscribe_in(
                &window_slider,
                window,
                move |this, _, event, window, cx| {
                    if let SliderEvent::Change(value) = event {
                        this.on_window_slider(index, value.start(), value.end(), window, cx);
                    }
                },
            ));
            self.channel_subs.push(cx.subscribe_in(
                &brightness_slider,
                window,
                move |this, _, event, window, cx| {
                    if let SliderEvent::Change(value) = event {
                        this.on_brightness_slider(index, value.start(), window, cx);
                    }
                },
            ));
        }
    }

    fn on_time_slider(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.series.is_none() || index == self.time_index {
            return;
        }
        self.stop_playback();
        self.time_index = index;
        self.schedule_load(window, cx);
    }

    fn show_time(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(series) = &self.series else {
            return;
        };
        if series.times.is_empty() {
            return;
        }
        self.time_index = index.min(series.times.len() - 1);
        self.sync_slider(window, cx);
        self.load_now(window, cx);
    }

    fn sync_slider(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let index = self.time_index as f32;
        self.time_slider.update(cx, |slider, cx| {
            slider.set_value(index, window, cx);
        });
    }

    fn step_time(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let length = self
            .series
            .as_ref()
            .map(|series| series.times.len())
            .unwrap_or(0);
        if length == 0 {
            return;
        }
        self.stop_playback();
        let length = length as isize;
        let mut index = self.time_index as isize + delta;
        if index < 0 {
            index = 0;
        }
        if index >= length {
            index = length - 1;
        }
        self.show_time(index as usize, window, cx);
    }

    fn step_position(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let length = self
            .series
            .as_ref()
            .map(|series| series.positions.len())
            .unwrap_or(0);
        if length == 0 {
            return;
        }
        let length = length as isize;
        let mut index = self.position_index as isize + delta;
        if index < 0 {
            index = 0;
        }
        if index >= length {
            index = length - 1;
        }
        self.position_index = index as usize;
        self.load_now(window, cx);
    }

    fn set_z(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.z_index = index;
        self.load_now(window, cx);
    }

    fn stop_playback(&mut self) {
        self.playing = false;
        self.play_task = None;
    }

    fn toggle_play(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.series.is_none() {
            return;
        }
        if self.playing {
            self.stop_playback();
            cx.notify();
            return;
        }
        self.playing = true;
        cx.notify();
        self.play_task = Some(cx.spawn_in(window, async move |this, cx| loop {
            let captured = this
                .update_in(cx, |view, window, cx| {
                    if !view.playing {
                        return None;
                    }
                    let series = view.series.clone()?;
                    if series.times.is_empty() {
                        return None;
                    }
                    view.time_index = (view.time_index + 1) % series.times.len();
                    view.sync_slider(window, cx);
                    view.revision = view.revision.wrapping_add(1);
                    let revision = view.revision;
                    view.loading = true;
                    view.capture(revision)
                })
                .ok()
                .flatten();
            let Some((revision, request)) = captured else {
                break;
            };
            let result = cx.background_spawn(async move { request.execute() }).await;
            let keep_going = this
                .update_in(cx, |view, window, cx| {
                    if view.revision != revision || !view.playing {
                        return false;
                    }
                    view.apply_moment(result, window, cx);
                    true
                })
                .unwrap_or(false);
            if !keep_going {
                break;
            }
            cx.background_spawn(async { std::thread::sleep(Duration::from_millis(40)) })
                .await;
        }));
    }

    fn capture(&self, revision: u64) -> Option<(u64, FrameRequest)> {
        let series = self.series.clone()?;
        let position = series.positions.get(self.position_index)?.value;
        let time = *series.times.get(self.time_index)?;
        let z = *series.zs.get(self.z_index)?;
        Some((
            revision,
            FrameRequest {
                series,
                position,
                time,
                z,
            },
        ))
    }

    fn load_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.revision = self.revision.wrapping_add(1);
        let revision = self.revision;
        let Some((_, request)) = self.capture(revision) else {
            return;
        };
        self.loading = true;
        cx.notify();
        self.load_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_spawn(async move { request.execute() }).await;
            this.update_in(cx, |view, window, cx| {
                if view.revision != revision {
                    return;
                }
                view.apply_moment(result, window, cx);
            })
            .ok();
        }));
    }

    fn schedule_load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.revision = self.revision.wrapping_add(1);
        let revision = self.revision;
        let Some((_, request)) = self.capture(revision) else {
            return;
        };
        self.loading = true;
        cx.notify();
        self.load_task = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_spawn(async { std::thread::sleep(Duration::from_millis(50)) })
                .await;
            let current = this
                .update(cx, |view, _| view.revision == revision)
                .unwrap_or(false);
            if !current {
                return;
            }
            let result = cx.background_spawn(async move { request.execute() }).await;
            this.update_in(cx, |view, window, cx| {
                if view.revision != revision {
                    return;
                }
                view.apply_moment(result, window, cx);
            })
            .ok();
        }));
    }

    fn apply_moment(
        &mut self,
        result: Result<Moment, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.loading = false;
        match result {
            Ok(moment) => {
                self.moment = Some(moment);
                self.apply_auto_contrast();
                self.sync_channel_sliders(window, cx);
                self.recomposite();
                self.refresh_status();
            }
            Err(error) => self.status = error,
        }
        cx.notify();
    }

    fn apply_auto_contrast(&mut self) {
        let updates: Vec<(u32, f32, f32)> = {
            let Some(moment) = &self.moment else {
                return;
            };
            self.channels
                .iter()
                .filter(|control| control.auto)
                .filter_map(|control| {
                    let loaded = moment
                        .channels
                        .iter()
                        .find(|channel| channel.index == control.index)?;
                    let (low, high) = percentile_range(&loaded.plane.pixels, 0.1, 99.9);
                    Some((control.index, low, high))
                })
                .collect()
        };
        for (index, low, high) in updates {
            if let Some(control) = self
                .channels
                .iter_mut()
                .find(|control| control.index == index)
            {
                control.low = low.max(1.0);
                control.high = high.max(control.low + 1.0).min(65535.0);
                control.auto = false;
            }
        }
    }

    fn recomposite(&mut self) {
        let Some(moment) = &self.moment else {
            return;
        };
        let mut planes = Vec::new();
        let mut settings = Vec::new();
        for loaded in &moment.channels {
            let Some(control) = self
                .channels
                .iter()
                .find(|control| control.index == loaded.index)
            else {
                continue;
            };
            planes.push(loaded.plane.clone());
            settings.push(control.render_settings());
        }
        let references: Vec<&Plane> = planes.iter().collect();
        let frame = composite(&references, &settings);
        if frame.width == 0 || frame.height == 0 {
            return;
        }
        self.frame_width = frame.width;
        self.frame_height = frame.height;
        self.rendered = Some(to_render_image(&frame));
        self.frame_epoch = self.frame_epoch.wrapping_add(1);
    }

    fn refresh_status(&mut self) {
        let Some(series) = &self.series else {
            return;
        };
        let position = series
            .positions
            .get(self.position_index)
            .map(|position| position.label.as_str())
            .unwrap_or("?");
        let time = series.times.get(self.time_index).copied().unwrap_or(0);
        let z = series.zs.get(self.z_index).copied().unwrap_or(0);
        let loading = if self.loading { " · loading" } else { "" };
        self.status = format!(
            "{position} · time {time} ({}/{}) · z {z} · {}×{}{loading}",
            self.time_index + 1,
            series.times.len(),
            self.frame_width,
            self.frame_height
        );
    }

    fn mutate_channel(
        &mut self,
        index: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut ChannelControl),
    ) {
        if let Some(control) = self
            .channels
            .iter_mut()
            .find(|control| control.index == index)
        {
            change(control);
        }
        self.sync_channel_sliders(window, cx);
        self.recomposite();
        cx.notify();
    }

    fn auto_channel(&mut self, index: u32, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(control) = self
            .channels
            .iter_mut()
            .find(|control| control.index == index)
        {
            control.auto = true;
        }
        self.apply_auto_contrast();
        self.sync_channel_sliders(window, cx);
        self.recomposite();
        cx.notify();
    }

    fn on_window_slider(
        &mut self,
        index: u32,
        black: f32,
        white: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let black = black.clamp(1.0, 65534.0);
        let white = white.clamp(black + 1.0, 65535.0);
        let unchanged = self.channels.iter().any(|control| {
            control.index == index
                && (control.low - black).abs() < 0.5
                && (control.high - white).abs() < 0.5
        });
        if unchanged {
            return;
        }
        self.mutate_channel(index, window, cx, |control| {
            control.auto = false;
            control.low = black;
            control.high = white;
        });
    }

    fn on_brightness_slider(
        &mut self,
        index: u32,
        brightness: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let brightness = brightness.clamp(0.0, 3.0);
        let unchanged = self
            .channels
            .iter()
            .any(|control| control.index == index && (control.gain - brightness).abs() < 0.005);
        if unchanged {
            return;
        }
        self.mutate_channel(index, window, cx, |control| control.gain = brightness);
    }

    fn sync_channel_sliders(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let snapshot: Vec<_> = self
            .channels
            .iter()
            .map(|channel| {
                (
                    channel.window_slider.clone(),
                    channel.low.max(1.0),
                    channel.high.max(channel.low + 1.0).min(65535.0),
                    channel.brightness_slider.clone(),
                    channel.gain.clamp(0.0, 3.0),
                )
            })
            .collect();
        for (window_slider, low, high, brightness_slider, gain) in snapshot {
            window_slider.update(cx, |slider, cx| {
                let value = slider.value();
                if (value.start() - low).abs() > 0.5 || (value.end() - high).abs() > 0.5 {
                    slider.set_value((low, high), window, cx);
                }
            });
            brightness_slider.update(cx, |slider, cx| {
                if (slider.value().start() - gain).abs() > 0.005 {
                    slider.set_value(gain, window, cx);
                }
            });
        }
    }
}

impl Render for BioViewer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let time_count = self
            .series
            .as_ref()
            .map(|series| series.times.len())
            .unwrap_or(0);
        div()
            .id("bioviewer")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &NextTime, window, cx| this.step_time(1, window, cx)))
            .on_action(cx.listener(|this, _: &PrevTime, window, cx| this.step_time(-1, window, cx)))
            .on_action(
                cx.listener(|this, _: &NextPosition, window, cx| this.step_position(1, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &PrevPosition, window, cx| {
                    this.step_position(-1, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &TogglePlay, window, cx| this.toggle_play(window, cx)))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x161616))
            .text_color(rgb(0xe8e8e8))
            .child(self.toolbar(cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_hidden()
                    .child(self.left_panel(cx))
                    .child(self.viewport())
                    .child(self.channel_panel(cx)),
            )
            .child(self.transport(time_count, cx))
    }
}

impl BioViewer {
    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .bg(rgb(0x202020))
            .border_b_1()
            .border_color(rgb(0x333333))
            .child(
                div()
                    .font_weight(gpui_kit::FontWeight::BOLD)
                    .child("BioViewer"),
            )
            .child(
                Button::new("up")
                    .label("Up")
                    .on_click(cx.listener(|this, _, _, cx| this.go_up(cx))),
            )
            .child(
                Button::new("choose")
                    .label("Choose folder")
                    .on_click(cx.listener(|this, _, _, cx| this.choose_folder(cx))),
            )
            .child(
                Button::new("open-series")
                    .primary()
                    .label(if self.has_display_settings {
                        "Open Micro-Manager series"
                    } else {
                        "Open series in this folder"
                    })
                    .on_click(cx.listener(|this, _, window, cx| this.begin_scan(window, cx))),
            )
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .text_color(rgb(0xb0b0b0))
                    .child(self.browse.display().to_string()),
            )
    }

    fn left_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let header = match self.left {
            Left::Browser => "Folders",
            Left::Positions => "Positions",
        };
        div()
            .w(px(300.0))
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(0x1c1c1c))
            .border_r_1()
            .border_color(rgb(0x333333))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .py_2()
                    .child(header)
                    .child(if self.series.is_some() {
                        div()
                            .flex()
                            .gap_1()
                            .child(Button::new("show-folders").label("Folders").on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.left = Left::Browser;
                                    cx.notify();
                                }),
                            ))
                            .child(Button::new("show-positions").label("Positions").on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.left = Left::Positions;
                                    cx.notify();
                                }),
                            ))
                            .into_any_element()
                    } else {
                        div().into_any_element()
                    }),
            )
            .child(match self.left {
                Left::Browser => self.browser_list(cx).into_any_element(),
                Left::Positions => self.position_list(cx).into_any_element(),
            })
    }

    fn browser_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("browser-list")
            .flex_1()
            .min_h(px(0.0))
            .overflow_y_scrollbar()
            .children(self.entries.iter().enumerate().map(|(index, entry)| {
                let label = if entry.is_dir {
                    format!("{}/", entry.name)
                } else {
                    entry.name.clone()
                };
                Button::new(SharedString::from(format!("entry-{index}")))
                    .label(label)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.activate_entry(index, window, cx);
                    }))
            }))
    }

    fn position_list(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let Some(labels) = self.series.as_ref().map(|series| {
            series
                .positions
                .iter()
                .map(|position| (position.label.clone(), position.value))
                .collect::<Vec<_>>()
        }) else {
            return div()
                .p_3()
                .text_color(rgb(0xaaaaaa))
                .child("Open a series to list positions.")
                .into_any_element();
        };
        let selected = self.position_index;
        div()
            .id("position-list")
            .flex_1()
            .min_h(px(0.0))
            .overflow_y_scrollbar()
            .children(
                labels
                    .into_iter()
                    .enumerate()
                    .map(|(index, (label, value))| {
                        let label = format!("{label}  ·  position {value:03}");
                        let label = if index == selected {
                            format!("● {label}")
                        } else {
                            label
                        };
                        Button::new(SharedString::from(format!("position-{index}")))
                            .label(label)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.position_index = index;
                                this.load_now(window, cx);
                            }))
                    }),
            )
            .into_any_element()
    }

    fn viewport(&self) -> impl IntoElement {
        let image = self.rendered.as_ref().map(|rendered| {
            let mut element = img(ImageSource::Render(rendered.clone()))
                .id(SharedString::from(format!("frame-{}", self.frame_epoch)))
                .object_fit(ObjectFit::Contain);
            if let Some(scale) = self.scale {
                if self.frame_width > 0 && self.frame_height > 0 {
                    element = element
                        .w(px(self.frame_width as f32 * scale))
                        .h(px(self.frame_height as f32 * scale));
                }
            } else {
                element = element.size_full();
            }
            element
        });
        div()
            .flex_1()
            .min_w(px(0.0))
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(0x000000))
            .child(
                div()
                    .id("viewport-scroll")
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_scrollbar()
                    .child(
                        image
                            .map(|image| image.into_any_element())
                            .unwrap_or_else(|| {
                                div()
                                    .p_6()
                                    .text_color(rgb(0x888888))
                                    .child("No frame yet.")
                                    .into_any_element()
                            }),
                    ),
            )
    }

    fn channel_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(px(360.0))
            .h_full()
            .flex()
            .flex_col()
            .id("channel-panel")
            .gap_2()
            .overflow_y_scrollbar()
            .bg(rgb(0x1c1c1c))
            .border_l_1()
            .border_color(rgb(0x333333))
            .p_3()
            .child("Channels")
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(0xaaaaaa))
                    .child("Black and white are the pixel values drawn as black and full color. Brightness mixes the channel into the image."),
            )
            .children({
                let mut cards = Vec::new();
                for channel in &self.channels {
                    cards.push(channel_card(channel, cx).into_any_element());
                }
                cards
            })
    }
}

fn channel_card(channel: &ChannelControl, cx: &mut Context<BioViewer>) -> impl IntoElement {
    let index = channel.index;
    let color = rgb_from(channel.color);
    div()
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .mb_2()
        .bg(rgb(0x262626))
        .border_1()
        .border_color(rgb(0x3a3a3a))
        .rounded_md()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .w(px(14.0))
                        .h(px(14.0))
                        .rounded_sm()
                        .bg(color)
                        .border_1()
                        .border_color(rgb(0x666666)),
                )
                .child(
                    Checkbox::new(SharedString::from(format!("enabled-{index}")))
                        .checked(channel.enabled)
                        .label(channel.name.clone())
                        .on_change(cx.listener(move |this, checked, window, cx| {
                            let checked = *checked;
                            this.mutate_channel(index, window, cx, |control| {
                                control.enabled = checked
                            });
                        })),
                ),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(0xaaaaaa))
                .child(channel.detail.clone()),
        )
        .child(slider_row(
            format!("Black {:.0}    White {:.0}", channel.low, channel.high),
            &channel.window_slider,
        ))
        .child(slider_row(
            format!("Brightness {:.2}", channel.gain),
            &channel.brightness_slider,
        ))
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_1()
                .children(PRESETS.iter().map(|(label, name, color, gain)| {
                    let name = (*name).to_string();
                    let color = *color;
                    let gain = *gain;
                    Button::new(SharedString::from(format!("preset-{index}-{label}")))
                        .label(*label)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let name = name.clone();
                            this.mutate_channel(index, window, cx, |control| {
                                control.name = name;
                                control.color = color;
                                control.gain = gain;
                            });
                        }))
                })),
        )
        .child(
            Button::new(SharedString::from(format!("auto-{index}")))
                .label("Auto")
                .on_click(
                    cx.listener(move |this, _, window, cx| this.auto_channel(index, window, cx)),
                ),
        )
}

fn slider_row(label: String, slider: &Entity<SliderState>) -> impl IntoElement {
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_xs().text_color(rgb(0xcccccc)).child(label))
        .child(Slider::new(slider).w_full())
}

impl BioViewer {
    fn transport(&self, time_count: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let z_count = self
            .series
            .as_ref()
            .map(|series| series.zs.len())
            .unwrap_or(0);
        div()
            .flex()
            .flex_col()
            .gap_2()
            .px_3()
            .py_2()
            .bg(rgb(0x202020))
            .border_t_1()
            .border_color(rgb(0x333333))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("prev-time")
                            .label("Prev")
                            .on_click(cx.listener(|this, _, window, cx| this.step_time(-1, window, cx))),
                    )
                    .child(
                        Button::new("play")
                            .primary()
                            .label(if self.playing { "Pause" } else { "Play" })
                            .on_click(cx.listener(|this, _, window, cx| this.toggle_play(window, cx))),
                    )
                    .child(
                        Button::new("next-time")
                            .label("Next")
                            .on_click(cx.listener(|this, _, window, cx| this.step_time(1, window, cx))),
                    )
                    .child(if time_count > 1 {
                        div().flex_1().child(Slider::new(&self.time_slider)).into_any_element()
                    } else {
                        div().flex_1().child("Single time point").into_any_element()
                    })
                    .child(
                        Button::new("zoom-out")
                            .label("−")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.scale = Some((this.scale.unwrap_or(1.0) / 1.25).max(0.1));
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("zoom-fit")
                            .label("Fit")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.scale = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("zoom-in")
                            .label("+")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.scale = Some((this.scale.unwrap_or(1.0) * 1.25).min(8.0));
                                cx.notify();
                            })),
                    )
                    .children((z_count > 1).then(|| {
                        div().flex().gap_1().children((0..z_count).map(|index| {
                            let label = if index == self.z_index {
                                format!("z{index} ●")
                            } else {
                                format!("z{index}")
                            };
                            Button::new(SharedString::from(format!("z-{index}")))
                                .label(label)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.set_z(index, window, cx)
                                }))
                        }))
                    })),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(0xc8c8c8))
                    .child(self.status.clone()),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(0x888888))
                    .child("Left and right step time. Up and down step position. Space plays. Brightfield starts dim so fluorescence stays visible."),
            )
    }
}

fn to_render_image(frame: &RgbaFrame) -> Arc<RenderImage> {
    let mut pixels = frame.pixels.clone();
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    let buffer = image::RgbaImage::from_raw(frame.width, frame.height, pixels)
        .expect("composite buffer size");
    Arc::new(RenderImage::new(vec![image::Frame::new(buffer)]))
}

fn rgb_from(color: [u8; 3]) -> gpui_kit::Rgba {
    rgb(((color[0] as u32) << 16) | ((color[1] as u32) << 8) | color[2] as u32)
}

fn default_root() -> PathBuf {
    let drive = PathBuf::from(r"E:\");
    if drive.is_dir() {
        drive
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    }
}

fn is_image_file(path: &std::path::Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase())
            .as_deref(),
        Some("tif" | "tiff" | "nd2" | "czi")
    )
}

fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            cx.bind_keys([
                KeyBinding::new("right", NextTime, None),
                KeyBinding::new("left", PrevTime, None),
                KeyBinding::new("down", NextPosition, None),
                KeyBinding::new("up", PrevPosition, None),
                KeyBinding::new("space", TogglePlay, None),
            ]);
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                        None,
                        size(px(1480.0), px(920.0)),
                        cx,
                    ))),
                    titlebar: Some(TitlebarOptions {
                        title: Some("BioViewer".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                cx,
                |window, cx| cx.new(|cx| BioViewer::new(window, cx)),
            )
            .expect("failed to open BioViewer");
        });
}
