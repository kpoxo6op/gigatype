use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::Sample;
use std::time::{Duration, Instant};

const OUTPUT_RATE: u32 = 16_000;
const DECODER_SILENCE_MS: u32 = 250;
const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

struct RecordingStats {
    sample_count: usize,
    duration_seconds: f64,
    peak: f32,
    rms: f32,
}

struct CaptureChunk {
    capture_start_nanos: u128,
    samples: Vec<f32>,
}

#[derive(Default)]
struct CaptureState {
    chunks: Vec<CaptureChunk>,
    last_callback_anchor: Option<(u128, Instant)>,
    latest_capture_end_nanos: u128,
}

type SharedCapture = Arc<(Mutex<CaptureState>, Condvar)>;

fn finalize_capture(
    chunks: &[CaptureChunk],
    cutoff_nanos: u128,
    sample_rate: u32,
    channels: usize,
    synthetic_silence_frames: usize,
) -> Vec<f32> {
    let mut samples = Vec::new();
    for chunk in chunks {
        if chunk.capture_start_nanos >= cutoff_nanos {
            continue;
        }
        let available_frames = chunk.samples.len() / channels;
        let duration_nanos = cutoff_nanos - chunk.capture_start_nanos;
        let frames_before_cutoff = duration_nanos
            .saturating_mul(sample_rate as u128)
            .div_ceil(1_000_000_000) as usize;
        let keep_samples = available_frames.min(frames_before_cutoff) * channels;
        samples.extend_from_slice(&chunk.samples[..keep_samples]);
    }
    samples.resize(samples.len() + synthetic_silence_frames * channels, 0.0);
    samples
}

fn stream_time_at(
    anchor_stream_nanos: u128,
    anchor_host_time: std::time::Instant,
    target_host_time: std::time::Instant,
) -> u128 {
    if target_host_time >= anchor_host_time {
        anchor_stream_nanos
            .saturating_add(target_host_time.duration_since(anchor_host_time).as_nanos())
    } else {
        anchor_stream_nanos
            .saturating_sub(anchor_host_time.duration_since(target_host_time).as_nanos())
    }
}

fn capture_config(supported: &cpal::SupportedStreamConfig) -> cpal::StreamConfig {
    let mut config = supported.config();
    if let cpal::SupportedBufferSize::Range { min, max } = *supported.buffer_size() {
        let twenty_ms = (supported.sample_rate() / 50).clamp(min, max);
        config.buffer_size = cpal::BufferSize::Fixed(twenty_ms);
    }
    config
}

pub struct Recorder {
    stream: Option<cpal::Stream>,
    capture: SharedCapture,
    input_rate: u32,
    channels: usize,
}

impl Recorder {
    pub fn start(preferred_device: Option<&str>) -> Result<Self, String> {
        let host = cpal::default_host();
        let device = if let Some(preferred) = preferred_device {
            host.input_devices()
                .map_err(|error| format!("could not enumerate microphones: {error}"))?
                .find(|device| {
                    device
                        .description()
                        .is_ok_and(|description| description.name() == preferred)
                })
                .ok_or_else(|| format!("microphone {preferred:?} is unavailable"))?
        } else {
            host.default_input_device()
                .ok_or_else(|| "no default microphone is available".to_string())?
        };
        let selected_name = device
            .description()
            .map(|description| description.name().to_string())
            .unwrap_or_else(|_| "unknown".to_string());
        let supported = device
            .supported_input_configs()
            .ok()
            .and_then(preferred_input_config)
            .map(Ok)
            .unwrap_or_else(|| device.default_input_config())
            .map_err(|error| format!("could not read microphone format: {error}"))?;
        let input_rate = supported.sample_rate();
        let channels = supported.channels() as usize;
        eprintln!(
            "GigaType microphone selected: {selected_name} ({input_rate} Hz, {channels} channel(s), {:?})",
            supported.sample_format()
        );
        let capture = Arc::new((Mutex::new(CaptureState::default()), Condvar::new()));
        let config = capture_config(&supported);
        let stream = match supported.sample_format() {
            cpal::SampleFormat::I8 => input_stream::<i8>(&device, config, &capture),
            cpal::SampleFormat::I16 => input_stream::<i16>(&device, config, &capture),
            cpal::SampleFormat::I24 => input_stream::<cpal::I24>(&device, config, &capture),
            cpal::SampleFormat::I32 => input_stream::<i32>(&device, config, &capture),
            cpal::SampleFormat::I64 => input_stream::<i64>(&device, config, &capture),
            cpal::SampleFormat::U8 => input_stream::<u8>(&device, config, &capture),
            cpal::SampleFormat::U16 => input_stream::<u16>(&device, config, &capture),
            cpal::SampleFormat::U24 => input_stream::<cpal::U24>(&device, config, &capture),
            cpal::SampleFormat::U32 => input_stream::<u32>(&device, config, &capture),
            cpal::SampleFormat::U64 => input_stream::<u64>(&device, config, &capture),
            cpal::SampleFormat::F32 => input_stream::<f32>(&device, config, &capture),
            cpal::SampleFormat::F64 => input_stream::<f64>(&device, config, &capture),
            format => return Err(format!("unsupported microphone sample format: {format:?}")),
        }
        .map_err(|error| format!("could not open microphone: {error}"))?;
        stream
            .play()
            .map_err(|error| format!("could not start microphone: {error}"))?;
        Ok(Self {
            stream: Some(stream),
            capture,
            input_rate,
            channels,
        })
    }

    pub fn stop(mut self, output: &Path) -> Result<(), String> {
        let cutoff_host_time = Instant::now();
        let cutoff_nanos = {
            let (lock, ready) = &*self.capture;
            let deadline = Instant::now() + DRAIN_TIMEOUT;
            let mut state = lock.lock().map_err(|error| error.to_string())?;
            while state.last_callback_anchor.is_none() && Instant::now() < deadline {
                let remaining = deadline.saturating_duration_since(Instant::now());
                let (next, timeout) = ready
                    .wait_timeout(state, remaining)
                    .map_err(|error| error.to_string())?;
                state = next;
                if timeout.timed_out() {
                    break;
                }
            }
            let Some((callback_stream_nanos, callback_host_time)) = state.last_callback_anchor
            else {
                return Err("the microphone produced no audio".into());
            };
            let cutoff =
                stream_time_at(callback_stream_nanos, callback_host_time, cutoff_host_time);
            while state.latest_capture_end_nanos < cutoff && Instant::now() < deadline {
                let remaining = deadline.saturating_duration_since(Instant::now());
                let (next, timeout) = ready
                    .wait_timeout(state, remaining)
                    .map_err(|error| error.to_string())?;
                state = next;
                if timeout.timed_out() {
                    break;
                }
            }
            cutoff
        };
        self.stream.take();
        let chunks = {
            let (lock, _) = &*self.capture;
            let mut state = lock.lock().map_err(|error| error.to_string())?;
            std::mem::take(&mut state.chunks)
        };
        if chunks.is_empty() {
            return Err("the microphone produced no audio".into());
        }
        let silence_frames = self.input_rate as usize * DECODER_SILENCE_MS as usize / 1_000;
        let interleaved = finalize_capture(
            &chunks,
            cutoff_nanos,
            self.input_rate,
            self.channels,
            silence_frames,
        );
        let mono = downmix(&interleaved, self.channels);
        let samples = resample_linear(&mono, self.input_rate, OUTPUT_RATE);
        let stats = recording_stats(&samples, OUTPUT_RATE);
        eprintln!(
            "GigaType microphone: {:.2}s, {} samples, peak {:.5}, RMS {:.5}",
            stats.duration_seconds, stats.sample_count, stats.peak, stats.rms
        );
        write_wav(output, &samples)
    }
}

fn preferred_input_config<I>(ranges: I) -> Option<cpal::SupportedStreamConfig>
where
    I: IntoIterator<Item = cpal::SupportedStreamConfigRange>,
{
    ranges.into_iter().find_map(|range| {
        (range.channels() == 1 && range.sample_format() == cpal::SampleFormat::I16)
            .then(|| range.try_with_sample_rate(OUTPUT_RATE))
            .flatten()
    })
}

fn input_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    capture: &SharedCapture,
) -> Result<cpal::Stream, cpal::Error>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    let sink = Arc::clone(capture);
    let sample_rate = config.sample_rate;
    let channels = config.channels as usize;
    device.build_input_stream(
        config,
        move |data: &[T], info| {
            let timestamp = info.timestamp();
            let capture_start_nanos = timestamp.capture.as_nanos();
            let frame_count = data.len() / channels;
            let duration_nanos =
                (frame_count as u128 * 1_000_000_000).div_ceil(sample_rate as u128);
            let chunk = CaptureChunk {
                capture_start_nanos,
                samples: data.iter().copied().map(f32::from_sample).collect(),
            };
            let (lock, ready) = &*sink;
            if let Ok(mut state) = lock.lock() {
                state.latest_capture_end_nanos = state
                    .latest_capture_end_nanos
                    .max(capture_start_nanos.saturating_add(duration_nanos));
                state.last_callback_anchor = Some((timestamp.callback.as_nanos(), Instant::now()));
                state.chunks.push(chunk);
                ready.notify_all();
            }
        },
        |error| eprintln!("GigaType microphone stream: {error}"),
        None,
    )
}

pub fn microphone_report() -> Result<String, String> {
    let host = cpal::default_host();
    let default_name = host
        .default_input_device()
        .and_then(|device| device.description().ok())
        .map(|description| description.name().to_string());
    let devices = host
        .input_devices()
        .map_err(|error| format!("could not enumerate microphones: {error}"))?;
    let mut report = format!("audio host: {}\n", host.id().name());
    for device in devices {
        let description = device
            .description()
            .map_err(|error| format!("could not describe a microphone: {error}"))?;
        let marker = if default_name.as_deref() == Some(description.name()) {
            "*"
        } else {
            " "
        };
        report.push_str(&format!("{marker} {}\n", description.name()));
    }
    Ok(report)
}

fn recording_stats(samples: &[f32], sample_rate: u32) -> RecordingStats {
    let squared = samples
        .iter()
        .map(|sample| (*sample as f64) * (*sample as f64))
        .sum::<f64>();
    RecordingStats {
        sample_count: samples.len(),
        duration_seconds: samples.len() as f64 / sample_rate as f64,
        peak: samples
            .iter()
            .map(|sample| sample.abs())
            .fold(0.0, f32::max),
        rms: if samples.is_empty() {
            0.0
        } else {
            (squared / samples.len() as f64).sqrt() as f32
        },
    }
}

fn downmix(input: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return input.to_vec();
    }
    input
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

pub fn resample_linear(input: &[f32], input_rate: u32, output_rate: u32) -> Vec<f32> {
    if input.is_empty() || input_rate == output_rate {
        return input.to_vec();
    }
    let output_len = input.len() as u64 * output_rate as u64 / input_rate as u64;
    (0..output_len as usize)
        .map(|index| {
            let source = index as f64 * input_rate as f64 / output_rate as f64;
            let left = source.floor() as usize;
            let right = (left + 1).min(input.len() - 1);
            let fraction = (source - left as f64) as f32;
            input[left] + (input[right] - input[left]) * fraction
        })
        .collect()
}

pub fn write_wav(path: &Path, samples: &[f32]) -> Result<(), String> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: OUTPUT_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec).map_err(|error| error.to_string())?;
    for sample in samples {
        let value = (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16;
        writer
            .write_sample(value)
            .map_err(|error| error.to_string())?;
    }
    writer.finalize().map_err(|error| error.to_string())
}

pub fn play_cue(listening: bool) -> Result<(), String> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| "no default audio output is available".to_string())?;
    let supported = device
        .default_output_config()
        .map_err(|error| format!("could not read audio output format: {error}"))?;
    let config = supported.config();
    let rate = config.sample_rate;
    let channels = config.channels as usize;
    let frequencies = if listening {
        [659.25_f32, 880.0]
    } else {
        [880.0_f32, 659.25]
    };
    let note_samples = (rate as f32 * 0.075) as usize;
    let mut mono = Vec::with_capacity(note_samples * 2);
    for frequency in frequencies {
        for index in 0..note_samples {
            let phase = 2.0 * std::f32::consts::PI * frequency * index as f32 / rate as f32;
            let position = index as f32 / note_samples as f32;
            let envelope = (position / 0.12).min(1.0) * ((1.0 - position) / 0.25).min(1.0);
            mono.push(phase.sin() * envelope.max(0.0) * 0.12);
        }
    }
    let samples = mono
        .into_iter()
        .flat_map(|sample| std::iter::repeat_n(sample, channels))
        .collect::<Vec<_>>();
    let duration = Duration::from_secs_f32(samples.len() as f32 / channels as f32 / rate as f32)
        + Duration::from_millis(20);
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => output_stream_f32(&device, config, samples)?,
        cpal::SampleFormat::I16 => output_stream_i16(&device, config, samples)?,
        cpal::SampleFormat::U16 => output_stream_u16(&device, config, samples)?,
        format => {
            return Err(format!(
                "unsupported audio output sample format: {format:?}"
            ))
        }
    };
    stream.play().map_err(|error| error.to_string())?;
    std::thread::sleep(duration);
    Ok(())
}

fn output_stream_f32(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    samples: Vec<f32>,
) -> Result<cpal::Stream, String> {
    let mut index = 0;
    device
        .build_output_stream(
            config,
            move |output: &mut [f32], _| {
                for value in output {
                    *value = samples.get(index).copied().unwrap_or(0.0);
                    index += 1;
                }
            },
            |error| eprintln!("GigaType sound output: {error}"),
            None,
        )
        .map_err(|error| error.to_string())
}

fn output_stream_i16(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    samples: Vec<f32>,
) -> Result<cpal::Stream, String> {
    let mut index = 0;
    device
        .build_output_stream(
            config,
            move |output: &mut [i16], _| {
                for value in output {
                    *value = (samples.get(index).copied().unwrap_or(0.0) * i16::MAX as f32) as i16;
                    index += 1;
                }
            },
            |error| eprintln!("GigaType sound output: {error}"),
            None,
        )
        .map_err(|error| error.to_string())
}

fn output_stream_u16(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    samples: Vec<f32>,
) -> Result<cpal::Stream, String> {
    let mut index = 0;
    device
        .build_output_stream(
            config,
            move |output: &mut [u16], _| {
                for value in output {
                    let sample = samples.get(index).copied().unwrap_or(0.0);
                    *value = ((sample * 0.5 + 0.5) * u16::MAX as f32) as u16;
                    index += 1;
                }
            },
            |error| eprintln!("GigaType sound output: {error}"),
            None,
        )
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampling_preserves_duration() {
        let input = vec![0.25; 48_000];
        let output = resample_linear(&input, 48_000, 16_000);
        assert_eq!(output.len(), 16_000);
        assert!(output.iter().all(|sample| (*sample - 0.25).abs() < 0.0001));
    }

    #[test]
    fn stereo_is_mixed_to_mono() {
        assert_eq!(downmix(&[1.0, -1.0, 0.5, 0.5], 2), [0.0, 0.5]);
    }

    #[test]
    fn recording_stats_report_duration_peak_and_rms() {
        let stats = recording_stats(&[0.5, -0.5, 0.0, 0.0], 2);
        assert_eq!(stats.sample_count, 4);
        assert!((stats.duration_seconds - 2.0).abs() < f64::EPSILON);
        assert!((stats.peak - 0.5).abs() < f32::EPSILON);
        assert!((stats.rms - 0.353_553_38).abs() < 0.000_001);
    }

    #[test]
    fn capture_negotiation_prefers_asr_native_pcm() {
        let ranges = vec![
            cpal::SupportedStreamConfigRange::new(
                2,
                44_100,
                48_000,
                cpal::SupportedBufferSize::Unknown,
                cpal::SampleFormat::F32,
            ),
            cpal::SupportedStreamConfigRange::new(
                1,
                8_000,
                48_000,
                cpal::SupportedBufferSize::Unknown,
                cpal::SampleFormat::I16,
            ),
        ];
        let selected = preferred_input_config(ranges).unwrap();
        assert_eq!(selected.channels(), 1);
        assert_eq!(selected.sample_rate(), OUTPUT_RATE);
        assert_eq!(selected.sample_format(), cpal::SampleFormat::I16);
    }

    #[test]
    fn timestamped_capture_trims_at_stop_and_adds_only_synthetic_silence() {
        let chunks = vec![CaptureChunk {
            capture_start_nanos: 0,
            samples: vec![0.5; 100],
        }];
        let audio = finalize_capture(&chunks, 75_000_000, 1_000, 1, 20);
        assert_eq!(audio.len(), 95);
        assert!(audio[..75].iter().all(|sample| *sample == 0.5));
        assert!(audio[75..].iter().all(|sample| *sample == 0.0));
    }

    #[test]
    fn f9_cutoff_is_mapped_onto_the_audio_stream_clock() {
        let callback = std::time::Instant::now();
        let cutoff = callback + Duration::from_millis(7);
        assert_eq!(stream_time_at(100_000_000, callback, cutoff), 107_000_000);
    }

    #[test]
    fn callback_arriving_after_f9_maps_back_to_the_exact_cutoff() {
        let cutoff = std::time::Instant::now();
        let callback = cutoff + Duration::from_millis(7);
        assert_eq!(stream_time_at(100_000_000, callback, cutoff), 93_000_000);
    }

    #[test]
    fn capture_uses_small_supported_buffers_instead_of_one_second_blocks() {
        let supported = cpal::SupportedStreamConfig::new(
            1,
            16_000,
            cpal::SupportedBufferSize::Range { min: 64, max: 4096 },
            cpal::SampleFormat::I16,
        );
        assert_eq!(
            capture_config(&supported).buffer_size,
            cpal::BufferSize::Fixed(320)
        );
    }
}
