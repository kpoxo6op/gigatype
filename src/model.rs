use std::env;
use std::f32::consts::PI;
use std::path::{Path, PathBuf};

use ort::session::Session;
use ort::value::Tensor;
use rustfft::num_complex::Complex32;
use rustfft::FftPlanner;
use sentencepiece_rust::SentencePieceProcessor;

use crate::audio::resample_linear;

const SAMPLE_RATE: u32 = 16_000;
const N_FFT: usize = 320;
const WIN_LENGTH: usize = 320;
const HOP_LENGTH: usize = 160;
const N_MELS: usize = 64;
const ENCODER_WIDTH: usize = 768;
const PREDICTOR_WIDTH: usize = 320;
const MAX_SYMBOLS_PER_FRAME: usize = 3;
const CHUNK_SAMPLES: usize = SAMPLE_RATE as usize * 22;

pub struct GigaAm {
    encoder: Session,
    decoder: Session,
    joint: Session,
    tokenizer: SentencePieceProcessor,
}

impl GigaAm {
    pub fn load_default() -> Result<Self, String> {
        Self::load(&model_dir())
    }

    pub fn load(dir: &Path) -> Result<Self, String> {
        let session = |name: &str| -> Result<Session, String> {
            Session::builder()
                .map_err(|error| error.to_string())?
                .with_intra_threads(8)
                .map_err(|error| error.to_string())?
                .commit_from_file(dir.join(name))
                .map_err(|error| format!("could not load {name}: {error}"))
        };
        let tokenizer = SentencePieceProcessor::open(dir.join("tokenizer.model"))
            .map_err(|error| format!("could not load tokenizer.model: {error}"))?;
        Ok(Self {
            encoder: session("v3_e2e_rnnt_encoder.onnx")?,
            decoder: session("v3_e2e_rnnt_decoder.onnx")?,
            joint: session("v3_e2e_rnnt_joint.onnx")?,
            tokenizer,
        })
    }

    pub fn transcribe_file(&mut self, path: &Path) -> Result<String, String> {
        let samples = read_wav(path)?;
        self.transcribe_samples(&samples)
    }

    pub fn transcribe_samples(&mut self, samples: &[f32]) -> Result<String, String> {
        if samples.len() > SAMPLE_RATE as usize * 25 {
            let mut parts = Vec::new();
            for chunk in samples.chunks(CHUNK_SAMPLES) {
                if chunk.len() < WIN_LENGTH {
                    continue;
                }
                let text = self.transcribe_short(chunk)?;
                if !text.trim().is_empty() {
                    parts.push(text);
                }
            }
            return Ok(parts.join(" "));
        }
        self.transcribe_short(samples)
    }

    fn transcribe_short(&mut self, samples: &[f32]) -> Result<String, String> {
        let (features, frames) = log_mel(samples)?;
        let feature_tensor = Tensor::from_array(([1, N_MELS, frames], features))
            .map_err(|error| error.to_string())?;
        let length_tensor =
            Tensor::from_array(([1], vec![frames as i64])).map_err(|error| error.to_string())?;
        let outputs = self
            .encoder
            .run(ort::inputs![
                "audio_signal" => feature_tensor,
                "length" => length_tensor,
            ])
            .map_err(|error| format!("GigaAM encoder failed: {error}"))?;
        let (encoded_shape, encoded_data) = outputs["encoded"]
            .try_extract_tensor::<f32>()
            .map_err(|error| error.to_string())?;
        if encoded_shape.len() != 3 || encoded_shape[1] as usize != ENCODER_WIDTH {
            return Err(format!(
                "unexpected GigaAM encoder shape: {encoded_shape:?}"
            ));
        }
        let encoded_frames = encoded_shape[2] as usize;
        let encoded = encoded_data.to_vec();
        let (_, encoded_lengths) = outputs["encoded_len"]
            .try_extract_tensor::<i32>()
            .map_err(|error| error.to_string())?;
        let active_frames = encoded_lengths.first().copied().unwrap_or_default().max(0) as usize;
        drop(outputs);
        self.decode_rnnt(&encoded, encoded_frames, active_frames)
    }

    fn decode_rnnt(
        &mut self,
        encoded: &[f32],
        encoded_frames: usize,
        active_frames: usize,
    ) -> Result<String, String> {
        let blank = self.tokenizer.piece_size() as i64;
        let mut tokens = Vec::<u32>::new();
        let mut last_label = blank;
        let mut hidden = vec![0.0_f32; PREDICTOR_WIDTH];
        let mut cell = vec![0.0_f32; PREDICTOR_WIDTH];
        let mut has_state = false;

        for frame in 0..active_frames.min(encoded_frames) {
            for _ in 0..MAX_SYMBOLS_PER_FRAME {
                let label = if has_state { last_label } else { blank };
                let decoder_outputs = self
                    .decoder
                    .run(ort::inputs![
                        "x" => Tensor::from_array(([1, 1], vec![label])).map_err(|e| e.to_string())?,
                        "h.1" => Tensor::from_array(([1, 1, PREDICTOR_WIDTH], hidden.clone())).map_err(|e| e.to_string())?,
                        "c.1" => Tensor::from_array(([1, 1, PREDICTOR_WIDTH], cell.clone())).map_err(|e| e.to_string())?,
                    ])
                    .map_err(|error| format!("GigaAM predictor failed: {error}"))?;
                let (_, prediction) = decoder_outputs["dec"]
                    .try_extract_tensor::<f32>()
                    .map_err(|error| error.to_string())?;
                let prediction = prediction.to_vec();
                let (_, next_hidden) = decoder_outputs["h"]
                    .try_extract_tensor::<f32>()
                    .map_err(|error| error.to_string())?;
                let next_hidden = next_hidden.to_vec();
                let (_, next_cell) = decoder_outputs["c"]
                    .try_extract_tensor::<f32>()
                    .map_err(|error| error.to_string())?;
                let next_cell = next_cell.to_vec();
                drop(decoder_outputs);

                let mut encoder_frame = Vec::with_capacity(ENCODER_WIDTH);
                for channel in 0..ENCODER_WIDTH {
                    encoder_frame.push(encoded[channel * encoded_frames + frame]);
                }
                let joint_outputs = self
                    .joint
                    .run(ort::inputs![
                        "enc" => Tensor::from_array(([1, ENCODER_WIDTH, 1], encoder_frame)).map_err(|e| e.to_string())?,
                        "dec" => Tensor::from_array(([1, PREDICTOR_WIDTH, 1], prediction)).map_err(|e| e.to_string())?,
                    ])
                    .map_err(|error| format!("GigaAM joint network failed: {error}"))?;
                let (_, logits) = joint_outputs["joint"]
                    .try_extract_tensor::<f32>()
                    .map_err(|error| error.to_string())?;
                let token = logits
                    .iter()
                    .enumerate()
                    .max_by(|(_, left), (_, right)| left.total_cmp(right))
                    .map(|(index, _)| index as i64)
                    .ok_or_else(|| "GigaAM joint network returned no logits".to_string())?;
                drop(joint_outputs);
                if token == blank {
                    break;
                }
                tokens.push(token as u32);
                last_label = token;
                hidden = next_hidden;
                cell = next_cell;
                has_state = true;
            }
        }
        self.tokenizer
            .decode(&tokens.iter().map(|&token| token as i32).collect::<Vec<_>>())
            .map_err(|error| format!("could not decode GigaAM tokens: {error}"))
    }
}

pub fn model_dir() -> PathBuf {
    env::var_os("GIGATYPE_MODEL")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local/share/gigatype/models/gigaam-v3-e2e-rnnt")
        })
}

pub fn model_is_complete(dir: &Path) -> bool {
    [
        "v3_e2e_rnnt_encoder.onnx",
        "v3_e2e_rnnt_decoder.onnx",
        "v3_e2e_rnnt_joint.onnx",
        "tokenizer.model",
    ]
    .iter()
    .all(|file| dir.join(file).is_file())
}

fn read_wav(path: &Path) -> Result<Vec<f32>, String> {
    let mut reader = hound::WavReader::open(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;
    let interleaved = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Int, 16) => reader
            .samples::<i16>()
            .map(|sample| sample.map(|value| value as f32 / 32768.0))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?,
        (hound::SampleFormat::Float, 32) => reader
            .samples::<f32>()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?,
        _ => return Err("audio must be 16-bit PCM or 32-bit float WAV".into()),
    };
    let mono = interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect::<Vec<_>>();
    Ok(resample_linear(&mono, spec.sample_rate, SAMPLE_RATE))
}

fn hz_to_mel(frequency: f32) -> f32 {
    2595.0 * (1.0 + frequency / 700.0).log10()
}

fn mel_to_hz(mel: f32) -> f32 {
    700.0 * (10_f32.powf(mel / 2595.0) - 1.0)
}

fn mel_filterbank() -> Vec<f32> {
    let frequencies = (0..=N_FFT / 2)
        .map(|index| index as f32 * SAMPLE_RATE as f32 / N_FFT as f32)
        .collect::<Vec<_>>();
    let mel_max = hz_to_mel(SAMPLE_RATE as f32 / 2.0);
    let points = (0..N_MELS + 2)
        .map(|index| mel_to_hz(mel_max * index as f32 / (N_MELS + 1) as f32))
        .collect::<Vec<_>>();
    let mut filters = vec![0.0; (N_FFT / 2 + 1) * N_MELS];
    for frequency in 0..frequencies.len() {
        for mel in 0..N_MELS {
            let lower = (frequencies[frequency] - points[mel]) / (points[mel + 1] - points[mel]);
            let upper =
                (points[mel + 2] - frequencies[frequency]) / (points[mel + 2] - points[mel + 1]);
            filters[frequency * N_MELS + mel] = lower.min(upper).max(0.0);
        }
    }
    filters
}

pub fn log_mel(samples: &[f32]) -> Result<(Vec<f32>, usize), String> {
    if samples.len() < WIN_LENGTH {
        return Err("audio is too short for GigaAM".into());
    }
    let frames = (samples.len() - WIN_LENGTH) / HOP_LENGTH + 1;
    let filters = mel_filterbank();
    let window = (0..WIN_LENGTH)
        .map(|index| 0.5 - 0.5 * (2.0 * PI * index as f32 / WIN_LENGTH as f32).cos())
        .collect::<Vec<_>>();
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(N_FFT);
    let mut features = vec![0.0_f32; N_MELS * frames];
    let mut spectrum = vec![Complex32::new(0.0, 0.0); N_FFT];
    for frame in 0..frames {
        let offset = frame * HOP_LENGTH;
        for index in 0..N_FFT {
            spectrum[index] = Complex32::new(samples[offset + index] * window[index], 0.0);
        }
        fft.process(&mut spectrum);
        for mel in 0..N_MELS {
            let power = spectrum[..=N_FFT / 2]
                .iter()
                .enumerate()
                .map(|(frequency, value)| value.norm_sqr() * filters[frequency * N_MELS + mel])
                .sum::<f32>();
            features[mel * frames + frame] = power.clamp(1e-9, 1e9).ln();
        }
    }
    Ok((features, frames))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_mel_matches_torchaudio_reference() {
        let samples = (0..640)
            .map(|index| (index as f32 * 2.0 * PI * 440.0 / SAMPLE_RATE as f32).sin())
            .collect::<Vec<_>>();
        let (features, frames) = log_mel(&samples).unwrap();
        let expected = [
            -8.437208, -7.652851, -7.653_48, -7.193342, -6.408985, -6.409614, -6.721_03, -6.247567,
            -6.246359, -6.964_29, -6.490827, -6.489619,
        ];
        assert_eq!(frames, 3);
        for (actual, expected) in features.iter().zip(expected) {
            assert!((actual - expected).abs() < 0.002, "{actual} != {expected}");
        }
    }
}
