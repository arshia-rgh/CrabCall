use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
    BufferSize, Device, SampleFormat, SampleRate, Stream, StreamConfig, SupportedBufferSize,
};
use ringbuf::consumer::Consumer;
use ringbuf::producer::Producer;
use ringbuf::traits::Split;
use ringbuf::{HeapCons, HeapProd, HeapRb};
use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct Audio {
    input_device: Device,
    output_device: Device,
    in_config: StreamConfig,
    out_config: StreamConfig,
    sample_rate: u32,
    out_channels: u16,
    cap_prod: Option<HeapProd<f32>>,
    cap_cons: Option<HeapCons<f32>>,
    play_prod: Option<HeapProd<f32>>,
    play_cons: Option<HeapCons<f32>>,
    cap_occupancy: Arc<AtomicUsize>,
    play_occupancy: Arc<AtomicUsize>,
    capacity: usize,
}

impl Audio {
    pub fn new() -> Result<Self, Box<dyn Error>> {
        let host = cpal::default_host();

        let input_device = host.default_input_device().ok_or("No input device")?;
        let output_device = host.default_output_device().ok_or("No output device")?;

        let preferred_rates: &[u32] = &[16_000, 48_000];

        let (sample_rate, out_channels) = pick_common_format(
            &input_device,
            &output_device,
            preferred_rates,
            SampleFormat::F32,
        )?;
        println!("{} sample rate", sample_rate);

        let in_config = StreamConfig {
            channels: 1,
            sample_rate: SampleRate(sample_rate),
            buffer_size: BufferSize::Default,
        };
        let out_config = StreamConfig {
            channels: out_channels,
            sample_rate: SampleRate(sample_rate),
            buffer_size: BufferSize::Default,
        };

        let capacity = (sample_rate as usize) * 5;
        let rb_cap = HeapRb::<f32>::new(capacity);
        let (cap_prod, cap_cons) = rb_cap.split();
        let rb_play = HeapRb::<f32>::new(capacity);
        let (play_prod, play_cons) = rb_play.split();

        Ok(Self {
            input_device,
            output_device,
            in_config,
            out_config,
            sample_rate,
            out_channels,
            cap_prod: Some(cap_prod),
            cap_cons: Some(cap_cons),
            play_prod: Some(play_prod),
            play_cons: Some(play_cons),
            cap_occupancy: Arc::new(AtomicUsize::new(0)),
            play_occupancy: Arc::new(AtomicUsize::new(0)),
            capacity,
        })
    }

    pub fn frame_samples(&self) -> usize {
        (self.sample_rate as usize) / 50
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn capture_occupancy_arc(&self) -> Arc<AtomicUsize> { Arc::clone(&self.cap_occupancy) }
    pub fn playback_occupancy_arc(&self) -> Arc<AtomicUsize> { Arc::clone(&self.play_occupancy) }

    pub fn take_capture_consumer(&mut self) -> Result<HeapCons<f32>, Box<dyn Error>> {
        self.cap_cons
            .take()
            .ok_or_else(|| "capture consumer already taken".into())
    }

    pub fn take_playback_producer(&mut self) -> Result<HeapProd<f32>, Box<dyn Error>> {
        self.play_prod
            .take()
            .ok_or_else(|| "playback producer already taken".into())
    }

    pub fn playback_capacity(&self) -> usize {
        self.capacity
    }

    pub fn start_capture(&mut self) -> Result<Stream, Box<dyn Error>> {
        let mut prod = self.cap_prod.take().ok_or("capture already started")?;
        let in_channels = self.in_config.channels as usize;
        let capacity = self.capacity;
        let occ = Arc::clone(&self.cap_occupancy);

        let stream = self.input_device.build_input_stream(
            &self.in_config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let mut pushed = 0usize;
                if in_channels == 1 {
                    for &s in data.iter() {
                        if prod.try_push(s).is_ok() {
                            pushed += 1;
                        } else {
                            break;
                        }
                    }
                } else {
                    let frames = data.len() / in_channels;
                    for f in 0..frames {
                        let mut sum = 0.0f32;
                        for ch in 0..in_channels {
                            sum += data[f * in_channels + ch];
                        }
                        let mono = sum / (in_channels as f32);
                        if prod.try_push(mono).is_ok() {
                            pushed += 1;
                        } else {
                            break;
                        }
                    }
                }

                if pushed > 0 {
                    let _ = occ.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |cur| {
                        let next = (cur + pushed).min(capacity);
                        Some(next)
                    });
                }
            },
            move |err| eprintln!("Input stream error: {err}"),
            None,
        )?;
        stream.play()?;
        Ok(stream)
    }

    pub fn start_playback(&mut self) -> Result<Stream, Box<dyn Error>> {
        let mut cons = self.play_cons.take().ok_or("playback already started")?;
        let out_channels = self.out_channels as usize;
        let occ = Arc::clone(&self.play_occupancy);

        let stream = self.output_device.build_output_stream(
            &self.out_config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let frames = data.len() / out_channels;
                let mut popped = 0usize;
                for f in 0..frames {
                    let sample_opt = cons.try_pop();
                    let s = match sample_opt {
                        Some(v) => {
                            popped += 1;
                            v
                        }
                        None => 0.0,
                    };
                    for ch in 0..out_channels {
                        data[f * out_channels + ch] = s;
                    }
                }
                if popped > 0 {
                    let _ = occ.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |cur| {
                        let next = cur.saturating_sub(popped);
                        Some(next)
                    });
                }
            },
            move |err| eprintln!("Output stream error: {err}"),
            None,
        )?;
        stream.play()?;
        Ok(stream)
    }
}

fn pick_common_format(
    input: &Device,
    output: &Device,
    preferred_rates: &[u32],
    fmt: SampleFormat,
) -> Result<(u32, u16), Box<dyn Error>> {
    let in_ranges = input
        .supported_input_configs()?
        .filter(|r| r.sample_format() == fmt)
        .collect::<Vec<_>>();
    let out_ranges = output
        .supported_output_configs()?
        .filter(|r| r.sample_format() == fmt)
        .collect::<Vec<_>>();

    let supports_in = |rate: u32| {
        in_ranges.iter().any(|r| {
            r.channels() >= 1 && r.min_sample_rate().0 <= rate && rate <= r.max_sample_rate().0
        })
    };
    let find_out = |rate: u32| -> Option<u16> {
        out_ranges.iter().find_map(|r| {
            if r.min_sample_rate().0 <= rate && rate <= r.max_sample_rate().0 {
                Some(r.channels())
            } else {
                None
            }
        })
    };

    for &rate in preferred_rates {
        if supports_in(rate) {
            if let Some(out_ch) = find_out(rate) {
                return Ok((rate, out_ch));
            }
        }
    }

    let out_def = output.default_output_config()?;
    let out_rate = out_def.sample_rate().0;
    if out_def.sample_format() == fmt && supports_in(out_rate) {
        return Ok((out_rate, out_def.channels()));
    }

    Err(
        "Failed to find common f32 format between input and output (try a different device/rate)"
            .into(),
    )
}
