use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Stream, StreamConfig};
use ringbuf::consumer::Consumer;
use ringbuf::producer::Producer;
use ringbuf::traits::{Observer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};
use std::error::Error;
use std::sync::{Arc, Mutex};

const BUFFER_SIZE: usize = 5 * 48000;

pub struct Audio {
    input_device: Device,
    output_device: Device,
    config: StreamConfig,
    buffer_producer: Arc<Mutex<HeapProd<f32>>>,
    buffer_consumer: Arc<Mutex<HeapCons<f32>>>,
}

impl Audio {
    pub fn new() -> Result<Self, Box<dyn Error>> {
        let host = cpal::default_host();

        let input_device = host
            .default_input_device()
            .ok_or("Failed to get default input device")?;
        let output_device = host
            .default_output_device()
            .ok_or("Failed to get default output device")?;
        let config = input_device.default_input_config()?.into();

        let ring_buffer = HeapRb::<f32>::new(BUFFER_SIZE);
        let (producer, consumer) = ring_buffer.split();

        Ok(Audio {
            input_device,
            output_device,
            config,
            buffer_producer: Arc::new(Mutex::new(producer)),
            buffer_consumer: Arc::new(Mutex::new(consumer)),
        })
    }

    pub fn start_capture(&self) -> Result<Stream, Box<dyn Error>> {
        let producer = Arc::clone(&self.buffer_producer);

        let input_stream = self.input_device.build_input_stream(
            &self.config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let mut prod = producer.lock().unwrap();
                for &sample in data {
                    if prod.is_full() {
                        break;
                    }
                    prod.try_push(sample).ok();
                }
            },
            move |err| {
                eprintln!("Input stream error: {}", err);
            },
            None,
        )?;

        input_stream.play()?;
        Ok(input_stream)
    }

    pub fn start_playback(&self) -> Result<Stream, Box<dyn Error>> {
        let consumer = Arc::clone(&self.buffer_consumer);

        let output_stream = self.output_device.build_output_stream(
            &self.config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let mut cons = consumer.lock().unwrap();
                for sample in data.iter_mut() {
                    *sample = cons.try_pop().unwrap_or(0.0);
                }
            },
            move |err| {
                eprintln!("Output stream error: {}", err);
            },
            None,
        )?;

        output_stream.play()?;
        Ok(output_stream)
    }
}
