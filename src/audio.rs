use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use audiopus::coder::Encoder;
use audiopus::{Application, Bitrate, Channels, SampleRate};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig, SupportedStreamConfigRange};
use tokio::sync::mpsc;
use tsclientlib::ClientId;
use tsproto_packets::packets::{AudioData, CodecType, InAudioBuf, OutAudio};

use crate::net::Mic;

const RATE: u32 = 48_000;
const FRAME: usize = 960;
const VAD_HOLD: Duration = Duration::from_millis(400);

pub enum Playback {
    Packet(ClientId, InAudioBuf),
    Sound(Sound),
    Volume(ClientId, f32),
    Reset,
}

#[derive(Clone, Copy)]
pub enum Sound {
    Join = 0,
    Leave = 1,
    SelfJoin = 2,
}

/// Two soft overlapping sine notes, a rising or falling "dö-düm".
fn chime(first: f32, second: f32) -> Vec<f32> {
    let gap = RATE as usize * 11 / 100;
    let len = RATE as usize * 45 / 100;
    let mut out = vec![0.0; gap + len];
    for (offset, freq) in [(0, first), (gap, second)] {
        for i in 0..len {
            let t = i as f32 / RATE as f32;
            let envelope = (1.0 - (-t * 400.0).exp()) * (-t * 9.0).exp();
            let tone = (std::f32::consts::TAU * freq * t).sin() + 0.25 * (std::f32::consts::TAU * 2.0 * freq * t).sin();
            out[offset + i] += 0.14 * envelope * tone;
        }
    }
    out
}

pub struct AtomicF32(AtomicU32);

impl AtomicF32 {
    fn new(v: f32) -> Self {
        Self(AtomicU32::new(v.to_bits()))
    }
    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Relaxed))
    }
    pub fn set(&self, v: f32) {
        self.0.store(v.to_bits(), Relaxed)
    }
}

pub struct Controls {
    pub ptt_mode: AtomicBool,
    pub ptt_down: AtomicBool,
    pub threshold_db: AtomicF32,
    pub level_db: AtomicF32,
    pub can_send: AtomicBool,
}

impl Controls {
    pub fn new(ptt_mode: bool, threshold_db: f32) -> Self {
        Self {
            ptt_mode: AtomicBool::new(ptt_mode),
            ptt_down: AtomicBool::new(false),
            threshold_db: AtomicF32::new(threshold_db),
            level_db: AtomicF32::new(-100.0),
            can_send: AtomicBool::new(false),
        }
    }
}

pub struct Audio {
    _input: cpal::Stream,
    _output: cpal::Stream,
}

impl Audio {
    /// `on_lost` fires from cpal's error callback, e.g. when the device is unplugged.
    pub fn start(
        controls: Arc<Controls>,
        mic_tx: mpsc::Sender<Mic>,
        playback: rtrb::Consumer<Playback>,
        on_lost: impl Fn() + Send + Clone + 'static,
    ) -> Result<Self> {
        let host = cpal::default_host();
        Ok(Self {
            _input: open_input(&host, controls, mic_tx, on_lost.clone())?,
            _output: open_output(&host, playback, on_lost)?,
        })
    }
}

/// Prefers a native 48 kHz float config so the common case needs no resampling.
fn pick_config(
    ranges: impl Iterator<Item = SupportedStreamConfigRange>,
    default: cpal::SupportedStreamConfig,
    channels_ok: impl Fn(u16) -> bool,
) -> StreamConfig {
    ranges
        .filter(|r| r.sample_format() == SampleFormat::F32 && channels_ok(r.channels()))
        .filter_map(|r| r.try_with_sample_rate(cpal::SampleRate(RATE)))
        .min_by_key(|c| c.channels())
        .map(|c| c.config())
        .unwrap_or_else(|| default.config())
}

/// Linear interpolation; plenty for 20 ms voice frames.
struct Resampler {
    step: f64,
    t: f64,
}

impl Resampler {
    fn new(from: u32, to: u32) -> Self {
        Self { step: from as f64 / to as f64, t: 0.0 }
    }
}

fn open_input(
    host: &cpal::Host,
    controls: Arc<Controls>,
    tx: mpsc::Sender<Mic>,
    on_lost: impl Fn() + Send + 'static,
) -> Result<cpal::Stream> {
    let device = host.default_input_device().ok_or_else(|| anyhow!("no microphone found"))?;
    let config = pick_config(device.supported_input_configs()?, device.default_input_config()?, |_| true);
    let channels = config.channels as usize;

    let mut encoder = Encoder::new(SampleRate::Hz48000, Channels::Mono, Application::Voip)?;
    encoder.set_bitrate(Bitrate::BitsPerSecond(40_000))?;
    encoder.set_inband_fec(true)?;
    encoder.set_packet_loss_perc(5)?;

    let mut rs = Resampler::new(config.sample_rate.0, RATE);
    let mut prev = 0f32;
    let mut frame = [0f32; FRAME];
    let mut filled = 0;
    let mut opus_out = [0u8; 1275];
    let mut last_loud = Instant::now() - VAD_HOLD;
    let mut was_sending = false;

    let stream = device.build_input_stream(
        &config,
        move |data: &[f32], _| {
            for chunk in data.chunks_exact(channels) {
                let x = chunk.iter().sum::<f32>() / channels as f32;
                while rs.t < 1.0 {
                    frame[filled] = prev + (x - prev) * rs.t as f32;
                    rs.t += rs.step;
                    filled += 1;
                    if filled < FRAME {
                        continue;
                    }
                    filled = 0;

                    let rms = (frame.iter().map(|s| s * s).sum::<f32>() / FRAME as f32).sqrt();
                    let db = 20.0 * rms.max(1e-5).log10();
                    controls.level_db.set(db);
                    let now = Instant::now();
                    if db >= controls.threshold_db.get() {
                        last_loud = now;
                    }
                    let gate = if controls.ptt_mode.load(Relaxed) {
                        controls.ptt_down.load(Relaxed)
                    } else {
                        now.duration_since(last_loud) < VAD_HOLD
                    };
                    let sending = gate && controls.can_send.load(Relaxed);

                    let payload = match (sending, was_sending) {
                        (true, _) => encoder.encode_float(&frame, &mut opus_out).ok().map(|n| &opus_out[..n]),
                        (false, true) => Some(&[][..]),
                        (false, false) => None,
                    };
                    was_sending = sending;
                    if let Some(data) = payload {
                        let packet = OutAudio::new(&AudioData::C2S { id: 0, codec: CodecType::OpusVoice, data });
                        let _ = tx.try_send((packet, !sending));
                    }
                }
                rs.t -= 1.0;
                prev = x;
            }
        },
        move |_| on_lost(),
        None,
    )?;
    stream.play().context("starting microphone")?;
    Ok(stream)
}

fn open_output(
    host: &cpal::Host,
    mut playback: rtrb::Consumer<Playback>,
    on_lost: impl Fn() + Send + 'static,
) -> Result<cpal::Stream> {
    let device = host.default_output_device().ok_or_else(|| anyhow!("no output device found"))?;
    let config = pick_config(device.supported_output_configs()?, device.default_output_config()?, |c| c >= 2);
    let channels = config.channels as usize;

    let mut handler = tsclientlib::audio::AudioHandler::<ClientId>::new(slog::Logger::root(slog::Discard, slog::o!()));
    let mut rs = Resampler::new(RATE, config.sample_rate.0);
    let mut src = [0f32; FRAME];
    let mut pos = src.len();
    let mut prev = [0f32; 2];
    let mut cur = [0f32; 2];
    let sounds = [chime(659.3, 880.0), chime(880.0, 659.3), chime(523.3, 784.0)];
    let mut sound: Option<(usize, usize)> = None;
    let mut volumes: std::collections::HashMap<ClientId, f32> = Default::default();

    let stream = device.build_output_stream(
        &config,
        move |data: &mut [f32], _| {
            while let Ok(msg) = playback.pop() {
                match msg {
                    Playback::Packet(id, packet) => {
                        if let (Ok(Some(id)), Some(v)) = (handler.handle_packet(id, packet), volumes.get(&id)) {
                            if let Some(q) = handler.get_mut_queues().get_mut(&id) {
                                q.volume = *v;
                            }
                        }
                    }
                    Playback::Volume(id, v) => {
                        volumes.insert(id, v);
                        if let Some(q) = handler.get_mut_queues().get_mut(&id) {
                            q.volume = v;
                        }
                    }
                    Playback::Sound(s) => sound = Some((s as usize, 0)),
                    Playback::Reset => handler.reset(),
                }
            }
            for out in data.chunks_exact_mut(channels) {
                while rs.t >= 1.0 {
                    if pos == src.len() {
                        src.fill(0.0);
                        handler.fill_buffer(&mut src);
                        if let Some((which, at)) = &mut sound {
                            let tail = &sounds[*which][*at..];
                            for (frame, v) in src.chunks_exact_mut(2).zip(tail) {
                                frame[0] += v;
                                frame[1] += v;
                            }
                            *at += (src.len() / 2).min(tail.len());
                            if *at == sounds[*which].len() {
                                sound = None;
                            }
                        }
                        pos = 0;
                    }
                    prev = cur;
                    cur = [src[pos], src[pos + 1]];
                    pos += 2;
                    rs.t -= 1.0;
                }
                let t = rs.t as f32;
                out.fill(0.0);
                out[0] = prev[0] + (cur[0] - prev[0]) * t;
                out[1] = prev[1] + (cur[1] - prev[1]) * t;
                rs.t += rs.step;
            }
        },
        move |_| on_lost(),
        None,
    )?;
    stream.play().context("starting playback")?;
    Ok(stream)
}
