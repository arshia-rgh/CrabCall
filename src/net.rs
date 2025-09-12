use std::collections::{BTreeMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::interval;

const MAGIC: [u8; 4] = *b"AUD0";
const VERSION: u8 = 1;
const HEADER_SIZE: usize = 24;
const MAX_UDP_PAYLOAD: usize = 1200;
const MIN_JITTER_DEPTH: usize = 3;
const MAX_JITTER_DEPTH: usize = 5;

#[derive(Clone, Debug)]
pub struct Stats {
    pub received: u64,
    pub played: u64,
    pub dropped_late: u64,
    pub reordered: u64,
    pub plc_frames: u64,
    pub avg_playout_delay_ms: f64,
}

impl Default for Stats {
    fn default() -> Self {
        Self {
            received: 0,
            played: 0,
            dropped_late: 0,
            reordered: 0,
            plc_frames: 0,
            avg_playout_delay_ms: 0.0,
        }
    }
}

#[derive(Debug)]
struct Header {
    seq: u32,
    ts_ms: u64,
    n_samples: u16,
    sample_rate: u32,
    fmt: u8,
}

fn now_ms() -> u64 {
    static START: once_cell::sync::Lazy<Instant> = once_cell::sync::Lazy::new(Instant::now);
    START.elapsed().as_millis() as u64
}

fn write_header(h: &Header, out: &mut [u8; HEADER_SIZE]) {
    out[0..4].copy_from_slice(&MAGIC);
    out[4] = VERSION;
    out[5] = h.fmt;
    out[6..10].copy_from_slice(&h.seq.to_be_bytes());
    out[10..18].copy_from_slice(&h.ts_ms.to_be_bytes());
    out[18..20].copy_from_slice(&h.n_samples.to_be_bytes());
    out[20..24].copy_from_slice(&h.sample_rate.to_be_bytes());
}

fn read_header(buf: &[u8]) -> Option<(Header, &[u8])> {
    if buf.len() < HEADER_SIZE || &buf[0..4] != MAGIC.as_ref() || buf[4] != VERSION {
        return None;
    }
    let fmt = buf[5];
    let seq = u32::from_be_bytes([buf[6], buf[7], buf[8], buf[9]]);
    let ts_ms = u64::from_be_bytes([
        buf[10], buf[11], buf[12], buf[13], buf[14], buf[15], buf[16], buf[17],
    ]);
    let n_samples = u16::from_be_bytes([buf[18], buf[19]]);
    let sample_rate = u32::from_be_bytes([buf[20], buf[21], buf[22], buf[23]]);
    let payload = &buf[HEADER_SIZE..];
    Some((
        Header {
            seq,
            ts_ms,
            n_samples,
            sample_rate,
            fmt,
        },
        payload,
    ))
}

pub struct JitterBuffer {
    window: BTreeMap<u32, (Header, Vec<i16>)>,
    next_seq: u32,
    target_depth: usize,
    last_frame: Vec<i16>,
    late_queue: VecDeque<u32>,
    stats: Stats,
}

impl JitterBuffer {
    pub fn new(initial_seq: u32) -> Self {
        Self {
            window: BTreeMap::new(),
            next_seq: initial_seq,
            target_depth: MIN_JITTER_DEPTH,
            last_frame: Vec::new(),
            late_queue: VecDeque::with_capacity(128),
            stats: Stats::default(),
        }
    }

    fn push(&mut self, h: Header, pcm: Vec<i16>) {
        self.stats.received += 1;

        if seq_lt(h.seq, self.next_seq) {
            self.stats.dropped_late += 1;
            self.late_queue.push_back(h.seq);
            if self.late_queue.len() > 256 {
                self.late_queue.pop_front();
            }
            return;
        }

        if let Some((&max_seq, _)) = self.window.iter().rev().next() {
            if seq_gt(h.seq, max_seq.wrapping_add(1)) {
                self.stats.reordered += 1;
            }
        }

        if self.window.len() >= MAX_JITTER_DEPTH + 2 {
            if let Some((&k, _)) = self.window.iter().rev().next() {
                self.window.remove(&k);
            }
        }

        self.window.insert(h.seq, (h, pcm));
    }

    pub fn pop_for_playout(&mut self) -> Vec<i16> {
        self.adjust_depth();

        if self.window.len() < self.target_depth && self.stats.played == 0 {
            let zeros = self.zero_frame_or_cached();
            self.stats.plc_frames += 1;
            self.stats.played += 1;
            self.next_seq = self.next_seq.wrapping_add(1);
            return zeros;
        }

        if let Some((h, frame)) = self.window.remove(&self.next_seq) {
            let now = now_ms();
            let d = now.saturating_sub(h.ts_ms) as f64;
            self.stats.avg_playout_delay_ms = if self.stats.played == 0 {
                d
            } else {
                0.9 * self.stats.avg_playout_delay_ms + 0.1 * d
            };

            self.last_frame = frame.clone();
            self.stats.played += 1;
            self.next_seq = self.next_seq.wrapping_add(1);
            frame
        } else {
            let plc = if self.last_frame.is_empty() {
                self.zero_frame_or_cached()
            } else {
                self.last_frame.clone()
            };
            self.stats.plc_frames += 1;
            self.stats.played += 1;
            self.next_seq = self.next_seq.wrapping_add(1);
            plc
        }
    }

    pub fn stats(&self) -> Stats {
        self.stats.clone()
    }

    fn zero_frame_or_cached(&self) -> Vec<i16> {
        if !self.last_frame.is_empty() {
            vec![0i16; self.last_frame.len()]
        } else {
            vec![0i16; 320]
        }
    }

    fn adjust_depth(&mut self) {
        if self.late_queue.len() >= 50 {
            let late = self.late_queue.len();
            self.late_queue.clear();
            if late > 5 && self.target_depth < MAX_JITTER_DEPTH {
                self.target_depth += 1;
            } else if late == 0 && self.target_depth > MIN_JITTER_DEPTH {
                self.target_depth -= 1;
            }
        }
    }
}

fn seq_lt(a: u32, b: u32) -> bool {
    a.wrapping_sub(b) > u32::MAX / 2
}
fn seq_gt(a: u32, b: u32) -> bool {
    seq_lt(b, a)
}

pub async fn run_sender(
    socket: std::sync::Arc<UdpSocket>,
    dest: SocketAddr,
    mut frames_rx: mpsc::Receiver<Vec<i16>>,
    sample_rate: u32,
    mut seq: u32,
) -> anyhow::Result<()> {
    while let Some(frame) = frames_rx.recv().await {
        let n_samples = frame.len() as u16;
        let payload_len = (n_samples as usize) * 2;
        let total = HEADER_SIZE + payload_len;

        if total > MAX_UDP_PAYLOAD {
            eprintln!(
                "Frame size {}B exceeds MTU {}B (rate {} Hz, samples {}). Drop.",
                total, MAX_UDP_PAYLOAD, sample_rate, n_samples
            );
            continue;
        }

        let h = Header {
            seq,
            ts_ms: now_ms(),
            n_samples,
            sample_rate,
            fmt: 1,
        };
        seq = seq.wrapping_add(1);

        let mut buf = vec![0u8; total];
        write_header(&h, (&mut buf[..HEADER_SIZE]).try_into().unwrap());

        for (i, s) in frame.iter().enumerate() {
            let b = s.to_le_bytes();
            buf[HEADER_SIZE + i * 2] = b[0];
            buf[HEADER_SIZE + i * 2 + 1] = b[1];
        }

        let _ = socket.send_to(&buf, dest).await?;
    }
    Ok(())
}

pub async fn run_receiver(
    socket: std::sync::Arc<UdpSocket>,
    jb: Arc<tokio::sync::Mutex<JitterBuffer>>,
) -> anyhow::Result<()> {
    let mut buf = vec![0u8; 2048];
    loop {
        let (n, _from) = socket.recv_from(&mut buf).await?;
        if n < HEADER_SIZE {
            continue;
        }
        if let Some((h, payload)) = read_header(&buf[..n]) {
            if h.fmt != 1 {
                continue;
            }
            let need = (h.n_samples as usize) * 2;
            if payload.len() != need {
                continue;
            }
            let mut pcm = vec![0i16; h.n_samples as usize];
            for i in 0..pcm.len() {
                let lo = payload[i * 2];
                let hi = payload[i * 2 + 1];
                pcm[i] = i16::from_le_bytes([lo, hi]);
            }
            let mut guard = jb.lock().await;
            guard.push(h, pcm);
        }
    }
}

pub async fn run_playout(
    jb: Arc<tokio::sync::Mutex<JitterBuffer>>,
    out_tx: mpsc::Sender<Vec<i16>>,
    frame_ms: u64, // 20
) -> anyhow::Result<()> {
    let mut tick = interval(Duration::from_millis(frame_ms));
    loop {
        tick.tick().await;
        let frame = {
            let mut guard = jb.lock().await;
            guard.pop_for_playout()
        };
        if out_tx.send(frame).await.is_err() {
            break;
        }
    }
    Ok(())
}
