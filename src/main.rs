use std::error::Error;

mod audio;
mod config;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let audio_obj = audio::Audio::new().unwrap();

    let _input_stream = audio_obj.start_capture().ok();
    let _output_stream = audio_obj.start_playback().ok();

    println!("Audio chat running. Speak into your microphone!");
    println!("Press Ctrl+C to stop...");

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            println!("Shutting down...")
        }
    }
    Ok(())
}
