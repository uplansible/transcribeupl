use rodio::{OutputStream, OutputStreamHandle, Sink, Source};
use std::sync::Arc;
use std::time::Duration;

use std::{fs::File, path::Path};

use symphonia::core::audio::{AudioBuffer, AudioBufferRef, Signal};
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::sample::{i24, u24, Sample};
use symphonia::default::{get_codecs, get_probe};

// Ensure the opus plugin is linked and self-registers.
#[allow(dead_code)]
#[allow(unused_imports)]
use symphonia_codec_opus as _;

#[derive(Debug, Clone)]
pub struct DecodedAudio {
    pub samples: Arc<Vec<f32>>, // interleaved
    pub sample_rate: u32,
    pub channels: u16,
    pub total_samples: usize, // interleaved samples count
    pub duration: Duration,
}

#[derive(thiserror::Error, Debug)]
pub enum AudioError {
    #[error("Unsupported channel count: {0}")]
    UnsupportedChannels(u16),
    #[error("Decode error: {0}")]
    DecodeError(String),
    #[error("IO error: {0}")]
    IoError(String),
}

pub fn decode_to_pcm_f32(path: &Path) -> Result<DecodedAudio, AudioError> {
    let file = File::open(path).map_err(|e| AudioError::IoError(e.to_string()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let probe = get_probe().format(
        &Default::default(),
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    );

    let mut probed = probe.map_err(|e| AudioError::DecodeError(format!("{e:?}")))?;
    let format = &mut probed.format;

    // Select the first audio track.
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| AudioError::DecodeError("No supported audio track found".into()))?
        .clone();

    let mut decoder = get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| AudioError::DecodeError(format!("{e:?}")))?;

    let mut sample_rate = track
        .codec_params
        .sample_rate
        .ok_or_else(|| AudioError::DecodeError("Missing sample rate".into()))?;
    let channels = track
        .codec_params
        .channels
        .ok_or_else(|| AudioError::DecodeError("Missing channel info".into()))?
        .count() as u16;

    if channels > 2 {
        return Err(AudioError::UnsupportedChannels(channels));
    }

    let mut out: Vec<f32> = Vec::new();

    loop {
        match format.next_packet() {
            Ok(packet) => match decoder.decode(&packet) {
                Ok(audio_buf) => match audio_buf {
                    AudioBufferRef::U8(buf) => {
                        push_map(buf.as_ref(), channels, &mut out, |x: u8| {
                            ((x as f32) - 128.0) / 128.0
                        })
                    }
                    AudioBufferRef::U16(buf) => {
                        push_map(buf.as_ref(), channels, &mut out, |x: u16| {
                            ((x as f32) - 32768.0) / 32768.0
                        })
                    }
                    AudioBufferRef::U24(buf) => {
                        push_map(buf.as_ref(), channels, &mut out, |x: u24| {
                            // 24-bit unsigned, right-aligned
                            let v = x.into_u32();
                            (v as f32 - 8_388_608.0) / 8_388_608.0
                        })
                    }
                    AudioBufferRef::U32(buf) => {
                        push_map(buf.as_ref(), channels, &mut out, |x: u32| {
                            (x as f32 - 2_147_483_648.0) / 2_147_483_648.0
                        })
                    }
                    AudioBufferRef::S8(buf) => {
                        push_map(buf.as_ref(), channels, &mut out, |x: i8| (x as f32) / 128.0)
                    }
                    AudioBufferRef::S16(buf) => {
                        push_map(buf.as_ref(), channels, &mut out, |x: i16| {
                            (x as f32) / 32768.0
                        })
                    }
                    AudioBufferRef::S24(buf) => {
                        push_map(buf.as_ref(), channels, &mut out, |x: i24| {
                            // 24-bit signed, right-aligned
                            let v = x.into_i32();
                            (v as f32) / 8_388_608.0
                        })
                    }
                    AudioBufferRef::S32(buf) => {
                        push_map(buf.as_ref(), channels, &mut out, |x: i32| {
                            (x as f32) / 2_147_483_648.0
                        })
                    }
                    AudioBufferRef::F32(buf) => push_f32(buf.as_ref(), channels, &mut out),
                    AudioBufferRef::F64(buf) => {
                        push_map(buf.as_ref(), channels, &mut out, |x: f64| x as f32)
                    }
                },
                Err(SymphError::IoError(err)) => {
                    if err.kind() == std::io::ErrorKind::UnexpectedEof {
                        break;
                    } else {
                        return Err(AudioError::DecodeError(format!("IO: {err}")));
                    }
                }
                Err(SymphError::ResetRequired) => {
                    // Re-create decoder with possibly updated params.
                    let params = decoder.codec_params().clone();
                    decoder = get_codecs()
                        .make(&params, &DecoderOptions::default())
                        .map_err(|e| AudioError::DecodeError(format!("Reset: {e:?}")))?;
                    sample_rate = params.sample_rate.unwrap_or(sample_rate);
                }
                Err(e) => return Err(AudioError::DecodeError(format!("{e:?}"))),
            },
            Err(SymphError::ResetRequired) => continue,
            Err(SymphError::IoError(err)) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                break
            }
            Err(e) => return Err(AudioError::DecodeError(format!("Probe read: {e:?}"))),
        }
    }

    let total_samples = out.len();
    let duration = if sample_rate > 0 && channels > 0 {
        Duration::from_secs_f64(total_samples as f64 / (sample_rate as f64 * channels as f64))
    } else {
        Duration::default()
    };

    log::info!(
        "Decoded {}: sr={} ch={} duration={:?}",
        path.display(),
        sample_rate,
        channels,
        duration
    );

    Ok(DecodedAudio {
        samples: Arc::new(out),
        sample_rate,
        channels,
        total_samples,
        duration,
    })
}

fn push_f32(buf: &AudioBuffer<f32>, ch: u16, out: &mut Vec<f32>) {
    let spec_ch = ch as usize;
    let frames = buf.frames();
    let chans = buf.spec().channels.count();
    // Interleave explicitly to ensure predictable layout.
    for f in 0..frames {
        for c in 0..spec_ch.min(chans) {
            out.push(buf.chan(c)[f]);
        }
        if chans == 1 && spec_ch == 2 {
            // Duplicate mono to stereo
            out.push(buf.chan(0)[f]);
        }
    }
}

fn push_map<T, F>(buf: &AudioBuffer<T>, ch: u16, out: &mut Vec<f32>, mut map: F)
where
    T: Sample + Copy,
    F: FnMut(T) -> f32,
{
    let spec_ch = ch as usize;
    let frames = buf.frames();
    let chans = buf.spec().channels.count();
    for f in 0..frames {
        for c in 0..spec_ch.min(chans) {
            out.push(map(buf.chan(c)[f]));
        }
        if chans == 1 && spec_ch == 2 {
            out.push(map(buf.chan(0)[f]));
        }
    }
}

pub struct Output {
    pub _stream: OutputStream,
    pub handle: OutputStreamHandle,
}

impl Output {
    pub fn new() -> Result<Self, String> {
        let (stream, handle) = OutputStream::try_default().map_err(|e| e.to_string())?;
        Ok(Self {
            _stream: stream,
            handle,
        })
    }
}

pub struct PcmSource {
    data: Arc<Vec<f32>>,
    channels: u16,
    sample_rate: u32,
    pos: f64,   // current sample index (interleaved)
    end: usize, // end index (interleaved)
    speed: f64, // 0.75, 1.0, 1.25, 1.5
}

impl PcmSource {
    pub fn new(
        data: Arc<Vec<f32>>,
        channels: u16,
        sample_rate: u32,
        start_index: usize,
        speed: f64,
    ) -> Self {
        let end = data.len();
        let pos = start_index as f64;
        Self {
            data,
            channels,
            sample_rate,
            pos,
            end,
            speed,
        }
    }
}

impl Iterator for PcmSource {
    type Item = f32;
    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.end as f64 {
            return None;
        }
        let i0 = self.pos.floor() as usize;
        let frac = self.pos - (i0 as f64);
        let s0 = self.data.get(i0).copied().unwrap_or(0.0);
        let s1 = self
            .data
            .get((i0 + 1).min(self.end.saturating_sub(1)))
            .copied()
            .unwrap_or(s0);
        let s = if frac > 0.0 {
            s0 + (s1 - s0) * frac as f32
        } else {
            s0
        };
        self.pos += self.speed;
        Some(s)
    }
}

impl Source for PcmSource {
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
        None
    }
}

pub struct Player {
    pub output: Output,
    pub sink: Option<Sink>,
    pub current_index: usize, // interleaved samples
    pub speed: f64,           // 0.75, 1.0, 1.25, 1.5
}

impl Player {
    pub fn new(output: Output) -> Self {
        Self {
            output,
            sink: None,
            current_index: 0,
            speed: 1.0,
        }
    }

    pub fn set_speed(&mut self, speed: f64, audio: &DecodedAudio) {
        self.speed = speed;
        // Rebuild sink at current index to apply new speed.
        let _ = self.start_from(audio, self.current_index);
    }

    pub fn start_from(&mut self, audio: &DecodedAudio, start_index: usize) -> Result<(), String> {
        // Stop existing sink
        if let Some(s) = self.sink.take() {
            s.stop();
        }
        self.current_index = start_index.min(audio.total_samples);
        let sink = Sink::try_new(&self.output.handle).map_err(|e| e.to_string())?;
        let src = PcmSource::new(
            audio.samples.clone(),
            audio.channels,
            audio.sample_rate,
            self.current_index,
            self.speed,
        );
        // Note: we can't update current_index from Source; UI estimates progress.
        sink.append(src);
        sink.play();
        self.sink = Some(sink);
        Ok(())
    }

    pub fn pause(&mut self) {
        if let Some(s) = &self.sink {
            s.pause();
        }
    }

    pub fn resume(&mut self) {
        if let Some(s) = &self.sink {
            s.play();
        }
    }

    pub fn is_playing(&self) -> bool {
        self.sink.as_ref().map(|s| !s.is_paused()).unwrap_or(false)
    }

    pub fn stop(&mut self) {
        if let Some(s) = self.sink.take() {
            s.stop();
        }
    }
}

// Helper to convert seconds to interleaved sample index.
pub fn seconds_to_index(seconds: f64, sr: u32, ch: u16) -> usize {
    let idx = (seconds * sr as f64 * ch as f64).floor();
    if idx.is_sign_negative() {
        0
    } else {
        idx as usize
    }
}
