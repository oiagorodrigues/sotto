//! Reader for the raw PCM frames the sidecar writes to file descriptor 3 (see
//! `sotto_protocol::sidecar` for the layout).

use sotto_protocol::Speaker;
use sotto_protocol::sidecar::PCM_FRAME_HEADER_BYTES;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::mpsc;

/// Ten seconds at 16 kHz. The sidecar writes buffers of tens of milliseconds, so a larger
/// count means the stream lost its framing.
pub const MAX_FRAME_SAMPLES: u32 = 160_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PcmFrame {
    pub speaker: Speaker,
    /// Milliseconds since the sidecar's t0 of the first sample.
    pub start_ms: u64,
    pub samples: Vec<i16>,
}

#[derive(Debug, thiserror::Error)]
pub enum PcmError {
    #[error("cannot read PCM frames: {0}")]
    Io(#[from] std::io::Error),
    #[error("unknown speaker tag {0} in a PCM frame")]
    BadSpeaker(u8),
    #[error("PCM frame of {0} samples is over the {MAX_FRAME_SAMPLES} sample limit")]
    TooLarge(u32),
}

fn speaker_from_tag(tag: u8) -> Result<Speaker, PcmError> {
    match tag {
        0 => Ok(Speaker::Me),
        1 => Ok(Speaker::Them),
        other => Err(PcmError::BadSpeaker(other)),
    }
}

/// Reads one frame. A clean end of stream before a header is `Ok(None)`; an end of stream in
/// the middle of a frame is an error.
pub async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<Option<PcmFrame>, PcmError> {
    let mut header = [0u8; PCM_FRAME_HEADER_BYTES];
    let mut filled = 0;
    while filled < header.len() {
        let n = reader.read(&mut header[filled..]).await?;
        if n == 0 {
            if filled == 0 {
                return Ok(None);
            }
            return Err(PcmError::Io(std::io::ErrorKind::UnexpectedEof.into()));
        }
        filled += n;
    }
    let speaker = speaker_from_tag(header[0])?;
    let mut start = [0u8; 8];
    start.copy_from_slice(&header[1..9]);
    let mut count = [0u8; 4];
    count.copy_from_slice(&header[9..13]);
    let count = u32::from_le_bytes(count);
    if count > MAX_FRAME_SAMPLES {
        return Err(PcmError::TooLarge(count));
    }
    let mut bytes = vec![0u8; count as usize * 2];
    reader.read_exact(&mut bytes).await?;
    let (pairs, _) = bytes.as_chunks::<2>();
    let samples = pairs.iter().map(|b| i16::from_le_bytes(*b)).collect();
    Ok(Some(PcmFrame {
        speaker,
        start_ms: u64::from_le_bytes(start),
        samples,
    }))
}

/// Forwards frames until the stream ends, errors, or the receiver is gone. A full channel
/// blocks the reader, which backs up the pipe: slow, never lossy.
pub async fn pump_frames<R: AsyncRead + Unpin>(mut reader: R, out: mpsc::Sender<PcmFrame>) {
    loop {
        match read_frame(&mut reader).await {
            Ok(Some(frame)) => {
                if out.send(frame).await.is_err() {
                    return;
                }
            }
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(error = %e, "PCM stream from the audio helper ended with an error");
                return;
            }
        }
    }
}

/// Encodes a frame the way the sidecar does. Used by tests and fakes.
#[must_use]
pub fn encode_frame(frame: &PcmFrame) -> Vec<u8> {
    let mut out = Vec::with_capacity(PCM_FRAME_HEADER_BYTES + frame.samples.len() * 2);
    out.push(frame.speaker.pcm_tag());
    out.extend_from_slice(&frame.start_ms.to_le_bytes());
    out.extend_from_slice(
        &u32::try_from(frame.samples.len())
            .unwrap_or(u32::MAX)
            .to_le_bytes(),
    );
    for s in &frame.samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(speaker: Speaker, start_ms: u64, samples: &[i16]) -> PcmFrame {
        PcmFrame {
            speaker,
            start_ms,
            samples: samples.to_vec(),
        }
    }

    #[tokio::test]
    async fn reads_crafted_bytes() {
        // Speaker them (1), start 0x0102 ms, 2 samples: 1 and -2.
        let bytes: Vec<u8> = vec![
            1, 0x02, 0x01, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0x01, 0x00, 0xFE, 0xFF,
        ];
        let mut r = &bytes[..];
        let f = read_frame(&mut r).await.unwrap().unwrap();
        assert_eq!(f, frame(Speaker::Them, 0x0102, &[1, -2]));
        assert!(read_frame(&mut r).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn reads_back_to_back_frames_split_across_reads() {
        let a = encode_frame(&frame(Speaker::Me, 5, &[7, 8, 9]));
        let b = encode_frame(&frame(Speaker::Them, 25, &[]));
        let mut all = a;
        all.extend(b);
        let (mut tx, mut rx) = tokio::io::duplex(8);
        let writer = tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            tx.write_all(&all).await.unwrap();
        });
        let f1 = read_frame(&mut rx).await.unwrap().unwrap();
        let f2 = read_frame(&mut rx).await.unwrap().unwrap();
        writer.await.unwrap();
        assert_eq!(f1, frame(Speaker::Me, 5, &[7, 8, 9]));
        assert_eq!(f2, frame(Speaker::Them, 25, &[]));
        assert!(read_frame(&mut rx).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn rejects_bad_speakers_oversized_counts_and_truncation() {
        let mut bad = encode_frame(&frame(Speaker::Me, 0, &[1]));
        bad[0] = 9;
        assert!(matches!(
            read_frame(&mut &bad[..]).await,
            Err(PcmError::BadSpeaker(9))
        ));

        let mut big = encode_frame(&frame(Speaker::Me, 0, &[]));
        big[9..13].copy_from_slice(&(MAX_FRAME_SAMPLES + 1).to_le_bytes());
        assert!(matches!(
            read_frame(&mut &big[..]).await,
            Err(PcmError::TooLarge(_))
        ));

        let full = encode_frame(&frame(Speaker::Me, 0, &[1, 2, 3]));
        for cut in [1, 12, 14, full.len() - 1] {
            assert!(
                matches!(read_frame(&mut &full[..cut]).await, Err(PcmError::Io(_))),
                "cut at {cut}"
            );
        }
    }

    #[tokio::test]
    async fn pump_stops_when_the_receiver_is_gone() {
        let bytes = encode_frame(&frame(Speaker::Me, 0, &[1]));
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        pump_frames(&bytes[..], tx).await;
    }
}
