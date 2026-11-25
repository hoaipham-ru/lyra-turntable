//! The device tap (ADR-004, S04): copy the exact bytes handed to the device API
//! out to a file for bit-perfect proof.
//!
//! The tap is a second lock-free SPSC ring. The real-time render thread pushes a
//! copy of every byte it hands to `IAudioRenderClient::GetBuffer` into the ring
//! with [`TapWriter::push`] — a non-blocking, non-allocating slice copy. A normal
//! thread running [`TapDrain::run`] pops from the ring and writes to the tap file.
//! No file I/O ever happens on the render thread.
//!
//! If the drain ever falls so far behind that the ring fills, the overflow bytes
//! are counted in [`TapWriter::dropped`] rather than blocking the render thread;
//! QA checks that the drop count is zero, which proves the tap captured the whole
//! stream. The tap ring is sized for ~2 seconds of audio so this does not happen
//! in practice.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use rtrb::{Consumer, Producer};

/// Render-thread side of the tap. Lives on the MMCSS render thread; every method
/// here is lock-free and allocation-free.
pub struct TapWriter {
    ring: Producer<u8>,
    dropped: Arc<AtomicU64>,
}

impl TapWriter {
    /// Copy `bytes` (exactly what was written to the device buffer) into the tap
    /// ring. Non-blocking: any bytes that do not fit are counted as dropped.
    ///
    /// Safe to call on the real-time thread: [`rtrb::Producer::push_partial_slice`]
    /// is a bounded memcpy with no allocation and no locking.
    pub fn push(&mut self, bytes: &[u8]) {
        let (_written, remainder) = self.ring.push_partial_slice(bytes);
        if !remainder.is_empty() {
            self.dropped.fetch_add(remainder.len() as u64, Ordering::Relaxed);
        }
    }

    /// Total bytes dropped because the drain could not keep up. Should be 0.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// A shared handle to the drop counter, so the owning [`crate::OutputHandle`]
    /// can report drops without touching the render thread.
    #[must_use]
    pub fn dropped_counter(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.dropped)
    }
}

/// Non-real-time side of the tap: drains the ring to a file.
pub struct TapDrain {
    ring: Consumer<u8>,
    file: BufWriter<File>,
    scratch: Vec<u8>,
}

impl TapDrain {
    /// Drain the ring to completion, writing every captured byte to `file`.
    ///
    /// Returns when the paired [`TapWriter`] has been dropped (the ring is
    /// abandoned) and every remaining byte has been flushed to disk. This runs on
    /// an ordinary thread, so it may allocate, block and do file I/O freely.
    pub fn run(mut self) -> io::Result<()> {
        loop {
            let got = self.drain_available()?;
            if self.ring.is_abandoned() {
                // Producer gone: make a final pass to catch anything in flight,
                // then stop.
                self.drain_available()?;
                break;
            }
            if got == 0 {
                // Nothing to do; yield briefly instead of spinning hot.
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
        self.file.flush()
    }

    /// Copy whatever is currently in the ring to the file. Returns bytes written.
    fn drain_available(&mut self) -> io::Result<usize> {
        let mut total = 0;
        loop {
            let (filled, _rest) = self.ring.pop_partial_slice(&mut self.scratch);
            let n = filled.len();
            if n == 0 {
                break;
            }
            self.file.write_all(filled)?;
            total += n;
        }
        Ok(total)
    }
}

/// Build a tap: a ring holding `capacity_bytes`, a [`TapWriter`] for the render
/// thread and a [`TapDrain`] that writes to `path`.
///
/// # Errors
/// Fails if the tap file cannot be created.
pub fn make_tap(path: &Path, capacity_bytes: usize) -> io::Result<(TapWriter, TapDrain)> {
    let (producer, consumer) = rtrb::RingBuffer::<u8>::new(capacity_bytes.max(1));
    let dropped = Arc::new(AtomicU64::new(0));
    let file = BufWriter::new(File::create(path)?);
    let writer = TapWriter { ring: producer, dropped };
    let drain = TapDrain { ring: consumer, file, scratch: vec![0u8; 64 * 1024] };
    Ok((writer, drain))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tap_captures_exact_bytes() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("lyra_tap_test_{}.raw", std::process::id()));
        let (mut writer, drain) = make_tap(&path, 1 << 16).unwrap();

        // Push several chunks, as the render thread would per device buffer.
        let chunk_a: Vec<u8> = (0..=255u8).collect();
        let chunk_b: Vec<u8> = (0..=255u8).rev().collect();
        writer.push(&chunk_a);
        writer.push(&chunk_b);
        assert_eq!(writer.dropped(), 0);

        // Drain on another thread, then drop the writer so the ring is abandoned.
        let handle = std::thread::spawn(move || drain.run().unwrap());
        drop(writer);
        handle.join().unwrap();

        let mut expected = chunk_a.clone();
        expected.extend_from_slice(&chunk_b);
        let got = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(got, expected);
    }

    #[test]
    fn overflow_is_counted_not_blocked() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("lyra_tap_overflow_{}.raw", std::process::id()));
        // Tiny ring: 4 bytes capacity. Pushing 10 must drop 6 (ring never holds
        // its full nominal capacity, but it certainly cannot hold 10).
        let (mut writer, _drain) = make_tap(&path, 4).unwrap();
        writer.push(&[0u8; 10]);
        assert!(writer.dropped() >= 6);
        std::fs::remove_file(&path).ok();
    }
}
