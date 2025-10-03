use ringbuf::consumer::Consumer;
use ringbuf::producer::Producer;
use std::error::Error;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::{Mutex, mpsc};
use tokio::time::interval;

mod audio;
mod codec;
mod config;
mod net;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut audio_obj = audio::Audio::new()?;
    let capture_consumer = audio_obj.take_capture_consumer()?;
    let mut play_producer = audio_obj.take_playback_producer()?;

    let _input_stream = audio_obj.start_capture()?;
    let _output_stream = audio_obj.start_playback()?;

    let sample_rate = audio_obj.sample_rate();
    let frame_sample = audio_obj.frame_samples();
    let occupancy = audio_obj.playback_occupancy_arc();
    let play_capacity = audio_obj.playback_capacity();

    println!("Audio chat running (mono).");
    println!("Sample rate: {sample_rate} Hz, frame: {frame_sample} samples (20 ms).");

    let local: SocketAddr = std::env::var("LOCAL")
        .unwrap_or_else(|_| "0.0.0.0:40000".to_string())
        .parse()?;
    let remote: SocketAddr = std::env::var("REMOTE")
        .unwrap_or_else(|_| "127.0.0.1:40000".to_string())
        .parse()?;
    println!("UDP bind: {local}, peer: {remote}");

    let sock = Arc::new(tokio::net::UdpSocket::bind(local).await?);

    let (tx_frames, rx_frames) = mpsc::channel::<Vec<i16>>(64);
    let (tx_playout, mut rx_playout) = mpsc::channel::<Vec<i16>>(64);

    let jb = Arc::new(Mutex::new(net::JitterBuffer::new(0)));

    // Choose codec: Opus or PCM
    let codec: Arc<Mutex<dyn codec::AudioCodec>> =
        Arc::new(Mutex::new(codec::OpusCodec::new(sample_rate)?));
    // let codec: Arc<Mutex<dyn codec::AudioCodec>> = Arc::new(Mutex::new(
    //     codec::PcmCodec::new(sample_rate, frame_sample)
    // ));

    // Spawn sender task
    {
        let sock_clone = Arc::clone(&sock);
        let codec_clone = Arc::clone(&codec);
        tokio::spawn(async move {
            if let Err(e) = net::run_sender(sock_clone, remote, rx_frames, codec_clone, 1).await {
                eprintln!("Sender error: {e}");
            }
        });
    }

    // Spawn receiver task
    {
        let sock_clone = Arc::clone(&sock);
        let jb_clone = Arc::clone(&jb);
        let codec_clone = Arc::clone(&codec);
        tokio::spawn(async move {
            if let Err(e) = net::run_receiver(sock_clone, jb_clone, codec_clone).await {
                eprintln!("Receiver error: {e}");
            }
        });
    }

    // Spawn playout task
    {
        let jb_clone = Arc::clone(&jb);
        tokio::spawn(async move {
            if let Err(e) = net::run_playout(jb_clone, tx_playout, 20).await {
                eprintln!("Playout error: {e}");
            }
        });
    }

    // Capture task: read from audio input -> encode -> send
    tokio::spawn({
        let mut cap_cons = capture_consumer;
        let tx = tx_frames.clone();
        async move {
            let mut tick = interval(Duration::from_millis(20));
            loop {
                tick.tick().await;
                let mut f = Vec::with_capacity(frame_sample);
                for _ in 0..frame_sample {
                    let s = cap_cons.try_pop().unwrap_or(0.0);
                    let s16 = (s * i16::MAX as f32).clamp(i16::MIN as f32, i16::MAX as f32) as i16;
                    f.push(s16);
                }
                if tx.send(f).await.is_err() {
                    break;
                }
            }
        }
    });

    // Playback task: receive decoded frames -> write to audio output
    let occ_push = Arc::clone(&occupancy);
    tokio::spawn(async move {
        while let Some(pcm16) = rx_playout.recv().await {
            let mut pushed = 0usize;
            for &s in pcm16.iter() {
                let s_f = s as f32 / i16::MAX as f32;
                if play_producer.try_push(s_f).is_ok() {
                    pushed += 1;
                }
            }
            if pushed > 0 {
                let _ = occ_push.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |cur| {
                    Some((cur + pushed).min(play_capacity))
                });
            }
        }
    });

    // Stats reporting
    let mut stats_tick = interval(Duration::from_millis(1000));
    let mut occ_tick = interval(Duration::from_millis(250));

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("Shutting down...");
                break;
            }
            _ = stats_tick.tick() => {
                let s = { jb.lock().await.stats() };
                println!(
                    "net: recv={}, played={}, plc={}, late_drop={}, reorders={}, avg_delay={:.1} ms",
                    s.received, s.played, s.plc_frames, s.dropped_late, s.reordered, s.avg_playout_delay_ms
                );
            }
            _ = occ_tick.tick() => {
                let occ_samples = occupancy.load(Ordering::Relaxed);
                let lag_ms = (occ_samples as f64) * 1000.0 / (sample_rate as f64);
                println!(
                    "audio: buffer={} samples (~{lag_ms:.1} ms)",
                    occ_samples
                );
            }
        }
    }

    Ok(())
}
