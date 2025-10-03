use anyhow::Result;
use opus::{Application, Bitrate, Channels, Decoder, Encoder};

pub const FMT_PCM: u8 = 1;
pub const FMT_OPUS: u8 = 2;

pub trait AudioCodec: Send {
    fn encode(&mut self, pcm: &[i16]) -> Result<Vec<u8>>;
    fn decode(&mut self, data: &[u8], expected_samples: usize) -> Result<Vec<i16>>;
    fn format_id(&self) -> u8;
    fn frame_samples(&self) -> usize;
    fn sample_rate(&self) -> u32;
}

pub struct OpusCodec {
    enc: Encoder,
    dec: Decoder,
    frame_samples: usize,
    max_packet: usize,
    sample_rate: u32,
}

impl OpusCodec {
    pub fn new(sample_rate: u32) -> Result<Self> {
        let mut enc = Encoder::new(sample_rate, Channels::Mono, Application::Voip)?;
        enc.set_bitrate(Bitrate::Bits(24_000))?;
        enc.set_vbr(true)?;
        enc.set_inband_fec(true)?;
        enc.set_packet_loss_perc(10)?;

        let dec = Decoder::new(sample_rate, Channels::Mono)?;

        Ok(Self {
            enc,
            dec,
            frame_samples: (sample_rate as usize) / 50, // 20ms frames
            max_packet: 1500,
            sample_rate,
        })
    }
}

impl AudioCodec for OpusCodec {
    fn encode(&mut self, pcm: &[i16]) -> Result<Vec<u8>> {
        debug_assert_eq!(pcm.len(), self.frame_samples);
        let mut buf = vec![0u8; self.max_packet];
        let n = self.enc.encode(pcm, &mut buf)?;
        buf.truncate(n);
        Ok(buf)
    }

    fn decode(&mut self, data: &[u8], expected_samples: usize) -> Result<Vec<i16>> {
        let mut pcm = vec![0i16; expected_samples];
        // fec=false: we have actual packet data
        let out = self.dec.decode(data, &mut pcm, false)?;
        pcm.truncate(out);
        Ok(pcm)
    }

    fn format_id(&self) -> u8 {
        FMT_OPUS
    }

    fn frame_samples(&self) -> usize {
        self.frame_samples
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
}

pub struct PcmCodec {
    frame_samples: usize,
    sample_rate: u32,
}

impl PcmCodec {
    pub fn new(sample_rate: u32, frame_samples: usize) -> Self {
        Self {
            frame_samples,
            sample_rate,
        }
    }
}

impl AudioCodec for PcmCodec {
    fn encode(&mut self, pcm: &[i16]) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; pcm.len() * 2];
        for (i, &sample) in pcm.iter().enumerate() {
            let bytes = sample.to_le_bytes();
            buf[i * 2] = bytes[0];
            buf[i * 2 + 1] = bytes[1];
        }
        Ok(buf)
    }

    fn decode(&mut self, data: &[u8], expected_samples: usize) -> Result<Vec<i16>> {
        let mut pcm = vec![0i16; expected_samples];
        let max_samples = (data.len() / 2).min(expected_samples);

        for i in 0..max_samples {
            pcm[i] = i16::from_le_bytes([data[i * 2], data[i * 2 + 1]]);
        }

        Ok(pcm)
    }

    fn format_id(&self) -> u8 {
        FMT_PCM
    }

    fn frame_samples(&self) -> usize {
        self.frame_samples
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
}
