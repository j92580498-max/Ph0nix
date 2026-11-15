// SPDX-License-Identifier: GPL-2.0-or-later
//! Native CPAL output for Linux and Windows, independent of firmware execution.
use anyhow::{Context, Result};
use cpal::{traits::{DeviceTrait, HostTrait, StreamTrait}, FromSample, Sample, SampleFormat, SizedSample};
use k750_native::worker::PlaybackPacket;
use k750_native::worker::DiagnosticsHandle;
use std::{collections::VecDeque, sync::{Arc, Mutex}};

#[derive(Default)]
struct Samples { frames: VecDeque<[f32; 2]>, held: Option<[f32; 2]>, phase: u32 }
pub struct AudioOutput { samples: Arc<Mutex<Samples>>, _stream: cpal::Stream }
impl AudioOutput {
    pub fn new(diagnostics: DiagnosticsHandle) -> Result<Self> {
        let device = cpal::default_host().default_output_device().context("No audio output device")?;
        let mut choices: Vec<_> = device.supported_output_configs()?.filter(|c|
            c.min_sample_rate().0 <= 48_000 && c.max_sample_rate().0 >= 48_000).collect();
        choices.sort_by_key(|c| (c.channels() != 2, c.sample_format() != SampleFormat::F32));
        let supported = choices.into_iter().next().map(|c| c.with_sample_rate(cpal::SampleRate(48_000)))
            .map(Ok).unwrap_or_else(|| device.default_output_config())?;
        let config = supported.config();
        let samples = Arc::new(Mutex::new(Samples::default()));
        let stream = match supported.sample_format() {
            SampleFormat::F32 => output::<f32>(&device, &config, samples.clone(), diagnostics)?,
            SampleFormat::I16 => output::<i16>(&device, &config, samples.clone(), diagnostics)?,
            SampleFormat::U16 => output::<u16>(&device, &config, samples.clone(), diagnostics)?,
            SampleFormat::I32 => output::<i32>(&device, &config, samples.clone(), diagnostics)?,
            SampleFormat::U32 => output::<u32>(&device, &config, samples.clone(), diagnostics)?,
            SampleFormat::F64 => output::<f64>(&device, &config, samples.clone(), diagnostics)?,
            format => anyhow::bail!("Unsupported speaker format: {format}"),
        };
        stream.play()?;
        Ok(Self { samples, _stream: stream })
    }
    pub fn enqueue(&self, packet: PlaybackPacket) {
        let mut samples = self.samples.lock().unwrap_or_else(|e| e.into_inner());
        if packet.discontinuity || samples.frames.len() + packet.pcm.len() / 4 > 9600 {
            samples.frames.clear(); samples.held = None; samples.phase = 0;
        }
        for frame in packet.pcm.chunks_exact(4) {
            samples.frames.push_back([
                i16::from_le_bytes([frame[0], frame[1]]) as f32 / 32768.0,
                i16::from_le_bytes([frame[2], frame[3]]) as f32 / 32768.0]);
        }
    }
}

fn output<T: Sample + SizedSample + FromSample<f32>>(
    device: &cpal::Device, config: &cpal::StreamConfig, samples: Arc<Mutex<Samples>>, diagnostics: DiagnosticsHandle,
) -> Result<cpal::Stream> {
    let channels = config.channels as usize;
    let rate = config.sample_rate.0;
    Ok(device.build_output_stream(config, move |out: &mut [T], _| {
        // Never wait in the speaker callback. Empty/busy queues produce silence.
        out.fill(T::from_sample(0.0));
        let Ok(mut samples) = samples.try_lock() else { return; };
        for frame in out.chunks_exact_mut(channels) {
            if samples.held.is_none() { samples.held = samples.frames.pop_front(); }
            if let Some(value) = samples.held {
                frame[0] = T::from_sample(if channels == 1 { (value[0] + value[1]) * 0.5 } else { value[0] });
                if channels > 1 { frame[1] = T::from_sample(value[1]); }
            }
            samples.phase += 48_000;
            while samples.phase >= rate {
                samples.phase -= rate;
                samples.held = samples.frames.pop_front();
            }
            if samples.held.is_none() { samples.phase = 0; }
        }
    }, move |error| diagnostics.log("error", "desktop-audio", error.to_string()), None)?)
}
