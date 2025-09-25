use anyhow::{anyhow, Context, Result};
use log::{error, info};
use rodio::{OutputStream, OutputStreamHandle, Sink, Source};
use std::sync::Arc;
use std::time::Duration;
use std::{fs::File, path::Path};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, Track};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::default::{get_codecs, get_probe};

// Ensure the Opus plugin is linked and self-registers.
#[allow(unused_imports)]
use symphonia_codec_opus as _;

#[derive(Debug, Clone)]
pub struct DecodedAudio {
    pub samples: Arc<Vec<f32>>, // interleaved
    pub sample_rate: u32,
    pub channels: u16,        // 1 or 2
    pub total_samples: usize, // interleaved count (frames * channels)
}

pub fn decode_to_f32_interleaved(path: &Path) -> Result<DecodedAudio> {
    let f = File::open(path).with_context(|| format!("Failed to open file: {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(f), Default::default());

    let hint = Hint::new();
    let probed = get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| anyhow!("Probe failed: {e}"))?;

    let mut format = probed.format;

    // choose best track
    let track = select_best_track(format.tracks())
        .ok_or_else(|| anyhow!("No supported audio track found"))?;
    let codec_params = track.codec_params.clone(); // Clone to avoid borrow issues

    let mut decoder = get_codecs()
        .make(&codec_params, &DecoderOptions::default())
        .map_err(|e| anyhow!("Decoder creation failed: {e}"))?;

    let sample_rate = codec_params
        .sample_rate
        .ok_or_else(|| anyhow!("Missing sample rate"))?;
    let channels = codec_params
        .channels
        .ok_or_else(|| anyhow!("Missing channel info"))?;
    let ch_count = channels.count();
    if ch_count == 0 {
        return Err(anyhow!("Zero channels"));
    }
    if ch_count > 2 {
        return Err(anyhow!(
            "Unsupported channel count: {} (only mono/stereo supported in Phase 1)",
            ch_count
        ));
    }

    let mut samples: Vec<f32> = Vec::new();
    let mut sample_buf: Option<SampleBuffer<f32>> = None;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                break
            }
            Err(SymphoniaError::ResetRequired) => {
                // A new decoder instance is required.
                decoder = get_codecs()
                    .make(&codec_params, &DecoderOptions::default())
                    .map_err(|e| anyhow!("Decoder reset failed: {e}"))?;
                continue;
            }
            Err(e) => return Err(anyhow!("Error reading packet: {e}")),
        };

        let decoded = match decoder.decode(&packet) {
            Ok(a) => a,
            Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                break
            }
            Err(SymphoniaError::DecodeError(e)) => {
                // Recoverable: skip bad packet.
                error!("Decode error (skipping packet): {e}");
                continue;
            }
            Err(e) => return Err(anyhow!("Decode error: {e}")),
        };

        let spec = *decoded.spec();

        // Ensure we have a SampleBuffer large enough for this packet.
        if sample_buf
            .as_ref()
            .map(|b| b.capacity() < decoded.capacity())
            .unwrap_or(true)
        {
            sample_buf = Some(SampleBuffer::<f32>::new(decoded.capacity() as u64, spec));
        }
        let sbuf = sample_buf.as_mut().unwrap();
        sbuf.copy_interleaved_ref(decoded);

        samples.extend_from_slice(sbuf.samples());
    }

    let total_samples = samples.len();
    info!(
        "Decoded: sr={} Hz, ch={}, frames={}, seconds≈{:.3}",
        sample_rate,
        ch_count,
        total_samples / ch_count,
        (total_samples as f64) / (sample_rate as f64) / (ch_count as f64)
    );

    Ok(DecodedAudio {
        samples: Arc::new(samples),
        sample_rate,
        channels: ch_count as u16,
        total_samples,
    })
}

fn select_best_track(tracks: &[Track]) -> Option<&Track> {
    // Pick the first track with a known codec type (not NULL).
    tracks
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
}

pub struct Output {
    pub _stream: OutputStream,
    pub handle: OutputStreamHandle,
}

impl Output {
    pub fn new() -> Result<Self> {
        let (_stream, handle) = OutputStream::try_default()?;
        Ok(Self { _stream, handle })
    }
}

pub struct SliceSource {
    data: Arc<Vec<f32>>,
    pos: usize, // interleaved index
    end: usize, // interleaved index
    channels: u16,
    sample_rate: u32, // adjusted for playback speed
}

impl SliceSource {
    pub fn new(
        data: Arc<Vec<f32>>,
        start: usize,
        channels: u16,
        base_sample_rate: u32,
        speed: f32,
    ) -> Self {
        let start = start.min(data.len());
        let end = data.len();
        let adj_sr = ((base_sample_rate as f32) * speed).round().max(1.0) as u32;
        Self {
            data,
            pos: start,
            end,
            channels,
            sample_rate: adj_sr,
        }
    }
}

impl Iterator for SliceSource {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.end {
            return None;
        }
        let v = self.data[self.pos];
        self.pos += 1;
        Some(v)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let rem = self.end.saturating_sub(self.pos);
        (rem, Some(rem))
    }
}

impl Source for SliceSource {
    fn current_frame_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> u16 {
        self.channels
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn total_duration(&self) -> Option<Duration> {
        if self.channels == 0 {
            return None;
        }
        let frames = (self.end.saturating_sub(self.pos)) as u64 / (self.channels as u64);
        Some(Duration::from_secs_f64(
            frames as f64 / (self.sample_rate as f64),
        ))
    }
}

pub struct Player {
    pub output: Output,
    pub sink: Option<Sink>,

    pub audio: Option<DecodedAudio>,
    pub file_path: Option<std::path::PathBuf>,

    pub playing: bool,
    pub speed: f32, // 0.75, 1.0, 1.25, 1.5

    // playback position management
    pub content_index: usize, // interleaved index when paused, or last seeked
    pub play_start_index: usize,
    pub play_start_instant: Option<std::time::Instant>,
}

impl Player {
    pub fn new() -> Result<Self> {
        Ok(Self {
            output: Output::new()?,
            sink: None,
            audio: None,
            file_path: None,
            playing: false,
            speed: 1.0,
            content_index: 0,
            play_start_index: 0,
            play_start_instant: None,
        })
    }

    pub fn load_file(&mut self, path: &Path) -> Result<()> {
        self.stop();
        let decoded = decode_to_f32_interleaved(path)?;
        self.audio = Some(decoded);
        self.file_path = Some(path.to_path_buf());
        self.content_index = 0;
        self.play_start_index = 0;
        self.play_start_instant = None;
        Ok(())
    }

    pub fn unload(&mut self) {
        self.stop();
        self.audio = None;
        self.file_path = None;
        self.content_index = 0;
        self.play_start_index = 0;
        self.play_start_instant = None;
    }

    pub fn total_frames(&self) -> usize {
        self.audio
            .as_ref()
            .map(|a| a.total_samples / (a.channels as usize))
            .unwrap_or(0)
    }

    pub fn current_index_interleaved(&self) -> usize {
        if !self.playing {
            return self.content_index;
        }
        let Some(audio) = &self.audio else {
            return 0;
        };
        let Some(start) = self.play_start_instant else {
            return self.content_index;
        };
        let elapsed = start.elapsed().as_secs_f64();
        let ch = audio.channels as usize;
        let delta = (elapsed * (audio.sample_rate as f64) * (self.speed as f64) * (ch as f64))
            .floor() as usize;
        let mut idx = self.play_start_index.saturating_add(delta);
        if idx > audio.total_samples {
            idx = audio.total_samples;
        }
        idx
    }

    pub fn current_time_secs(&self) -> (u64, u64) {
        if let Some(audio) = &self.audio {
            let ch = audio.channels as usize;
            let idx = self.current_index_interleaved();
            let frames = idx / ch;
            let total_frames = audio.total_samples / ch;
            let cur_secs = frames as f64 / audio.sample_rate as f64;
            let total_secs = total_frames as f64 / audio.sample_rate as f64;
            (cur_secs.floor() as u64, total_secs.floor() as u64)
        } else {
            (0, 0)
        }
    }

    fn rebuild_sink_from(&mut self, start_idx: usize) {
        if let Some(audio) = &self.audio {
            // Drop existing sink
            self.sink.take();

            let sink = Sink::try_new(&self.output.handle).expect("Failed to create Sink");
            // Build a zero-copy source view from the current index
            let source = SliceSource::new(
                audio.samples.clone(),
                start_idx,
                audio.channels,
                audio.sample_rate,
                self.speed,
            );
            sink.append(source);
            sink.play();

            self.sink = Some(sink);
            self.play_start_index = start_idx;
            self.play_start_instant = Some(std::time::Instant::now());
            self.playing = true;
        }
    }

    pub fn play_from_current(&mut self) {
        let idx = self.content_index;
        self.rebuild_sink_from(idx);
    }

    pub fn pause(&mut self) {
        if self.playing {
            // Update content_index to current
            let idx = self.current_index_interleaved();
            self.content_index = idx;
            if let Some(sink) = self.sink.take() {
                sink.stop();
            }
            self.playing = false;
            self.play_start_instant = None;
        }
    }

    pub fn stop(&mut self) {
        if let Some(sink) = self.sink.take() {
            sink.stop();
        }
        self.playing = false;
        self.play_start_instant = None;
    }

    pub fn seek_seconds(&mut self, delta_seconds: i64) {
        if let Some(audio) = &self.audio {
            let ch = audio.channels as usize;
            let total = audio.total_samples;
            let delta_samples = ((delta_seconds as f64) * (audio.sample_rate as f64) * (ch as f64))
                .floor() as isize;
            let base_idx = self.current_index_interleaved() as isize;
            let mut new_idx = base_idx + delta_samples;
            if new_idx < 0 {
                new_idx = 0;
            }
            if new_idx as usize > total {
                new_idx = total as isize;
            }
            self.content_index = new_idx as usize;

            if self.playing {
                self.rebuild_sink_from(self.content_index);
            }
        }
    }

    pub fn set_speed(&mut self, speed: f32) {
        self.speed = speed.max(0.1);
        if self.playing {
            // Continue from current content position under new speed.
            let idx = self.current_index_interleaved();
            self.content_index = idx;
            self.rebuild_sink_from(self.content_index);
        }
    }

    pub fn clamp_at_end_if_needed(&mut self) {
        if let Some(total) = self.audio.as_ref().map(|a| a.total_samples) {
            let idx = self.current_index_interleaved();
            if idx >= total {
                // Stop playback at end
                self.pause();
                self.content_index = total;
            }
        }
    }
}
