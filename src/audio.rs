use cpal::traits::{DeviceTrait, HostTrait};
use cpal::{Device, StreamConfig};
use ringbuf::traits::Split;
use ringbuf::{HeapCons, HeapProd, HeapRb};
use std::error::Error;
use std::sync::{Arc, Mutex};

const BUFFER_SIZE: usize = 48000;

struct Audio {
    input_device: Device,
    output_device: Device,
    config: StreamConfig,
    buffer_producer: Arc<Mutex<HeapProd<f32>>>,
    buffer_consumer: Arc<Mutex<HeapCons<f32>>>,
}

impl Audio {
    fn new() -> Result<Self, Box<dyn Error>> {
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
}
