use opus::{Application, Bitrate, Channels, Decoder, Encoder};

pub const FMT_PCM: u8 = 1;
pub const FMT_OPUS: u8 = 2;

pub struct OpusCodec {
    enc: Encoder,
    dec: Decoder,
    frame_samples: usize,
    max_packet: usize,
    sample_rate: u32,
}

impl OpusCodec {
    pub fn new(sample_rate: u32) -> anyhow::Result<Self> {
        let mut enc = Encoder::new(sample_rate, Channels::Mono, Application::Voip)?;
        enc.set_bitrate(Bitrate::Bits(24_000))?;
        enc.set_vbr(true)?;
        // enc.set_complexity(6)?;
        enc.set_inband_fec(true)?;
        enc.set_packet_loss_perc(10)?;
        let dec = Decoder::new(sample_rate, Channels::Mono)?;
        Ok(Self {
            enc,
            dec,
            frame_samples: (sample_rate as usize) / 50,
            max_packet: 1500,
            sample_rate,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    pub fn frame_samples(&self) -> usize {
        self.frame_samples
    }

    pub fn encode(&mut self, pcm: &[i16]) -> anyhow::Result<Vec<u8>> {
        debug_assert_eq!(pcm.len(), self.frame_samples);
        let mut buf = vec![0u8; self.max_packet];
        let n = self.enc.encode(pcm, &mut buf)?;
        buf.truncate(n);
        Ok(buf)
    }

    pub fn decode(&mut self, packet: &[u8], expected_samples: usize) -> anyhow::Result<Vec<i16>> {
        let mut pcm = vec![0i16; expected_samples];
        let out = self.dec.decode(packet, &mut pcm, false)?;
        pcm.truncate(out);
        Ok(pcm)
    }
}
