//! The holder's final input writer and its independent revocation channel.

use std::fs::File;
use std::time::Duration;

use anyhow::Context;
use tokio::io::unix::AsyncFd;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use super::wire::{self, InputEpoch};

const INPUT_QUEUE: usize = 16;
const RESET_TIMEOUT: Duration = Duration::from_secs(10);

struct Input {
    epoch: InputEpoch,
    bytes: Vec<u8>,
}

#[derive(Clone)]
pub(super) struct Gate {
    desired: watch::Sender<InputEpoch>,
    applied: watch::Receiver<InputEpoch>,
    input: mpsc::Sender<Input>,
}

/// Owns tasks so an in-process holder cannot leak its PTY descriptor on exit.
pub(super) struct Service {
    pub gate: Gate,
    writer: JoinHandle<()>,
    control: JoinHandle<()>,
}

impl Drop for Service {
    fn drop(&mut self) {
        self.writer.abort();
        self.control.abort();
    }
}

impl Service {
    pub fn start(fd: AsyncFd<File>, listener: UnixListener) -> Self {
        let (desired, changes) = watch::channel(InputEpoch::default());
        let (applied, confirmed) = watch::channel(InputEpoch::default());
        let (input, queue) = mpsc::channel(INPUT_QUEUE);
        let gate = Gate {
            desired,
            applied: confirmed,
            input,
        };
        let writer = tokio::spawn(write_loop(fd, queue, changes, applied));
        let control = tokio::spawn(control_loop(listener, gate.clone()));
        Self {
            gate,
            writer,
            control,
        }
    }
}

impl Gate {
    fn advance(&self, epoch: InputEpoch) {
        self.desired.send_if_modified(|current| {
            if epoch > *current {
                *current = epoch;
                true
            } else {
                false
            }
        });
    }

    pub async fn legacy(&self, connection: u64, bytes: Vec<u8>) {
        let epoch = InputEpoch {
            connection,
            browser: 0,
        };
        self.advance(epoch);
        self.queue(epoch, bytes).await;
    }

    pub async fn queue(&self, epoch: InputEpoch, bytes: Vec<u8>) {
        let mut current = self.desired.subscribe();
        if *current.borrow_and_update() != epoch {
            return;
        }
        tokio::select! {
            biased;
            _ = current.changed() => {}
            result = self.input.send(Input { epoch, bytes }) => {
                if result.is_err() { warn!("holder input writer is gone"); }
            }
        }
    }

    async fn reset(&self, epoch: InputEpoch) -> anyhow::Result<()> {
        let mut applied = self.applied.clone();
        self.advance(epoch);
        loop {
            if *applied.borrow_and_update() >= epoch {
                return Ok(());
            }
            applied
                .changed()
                .await
                .context("holder input writer stopped before reset")?;
        }
    }
}

async fn write_loop(
    fd: AsyncFd<File>,
    mut queue: mpsc::Receiver<Input>,
    mut current: watch::Receiver<InputEpoch>,
    applied: watch::Sender<InputEpoch>,
) {
    loop {
        let input = tokio::select! {
            biased;
            changed = current.changed() => {
                if changed.is_err() { return; }
                applied.send_replace(*current.borrow_and_update());
                continue;
            }
            input = queue.recv() => match input { Some(input) => input, None => return },
        };
        if *current.borrow() != input.epoch {
            continue;
        }
        // A reset drops the pending PTY write before publishing its acknowledgement.
        // Bytes already delivered to the host process cannot be recalled; queued
        // frames and unwritten suffixes cannot cross this barrier.
        let reset = tokio::select! {
            biased;
            _ = current.changed() => true,
            result = super::holder::pty_write(&fd, &input.bytes) => {
                if let Err(error) = result { debug!("terminal input write failed: {error}"); }
                false
            }
        };
        if reset {
            applied.send_replace(*current.borrow_and_update());
        }
    }
}

async fn control_loop(listener: UnixListener, gate: Gate) {
    loop {
        let (mut stream, _) = match listener.accept().await {
            Ok(stream) => stream,
            Err(error) => {
                warn!("holder input control accept failed: {error}");
                return;
            }
        };
        match tokio::time::timeout(RESET_TIMEOUT, reset_request(&mut stream, &gate)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => warn!("holder input reset failed: {error}"),
            Err(_) => warn!("holder input reset timed out"),
        }
    }
}

async fn reset_request(stream: &mut UnixStream, gate: &Gate) -> anyhow::Result<()> {
    let frame = wire::read_frame(stream)
        .await?
        .context("missing input reset")?;
    anyhow::ensure!(frame.kind == wire::RESET_INPUT, "expected input reset");
    let epoch = InputEpoch::decode(&frame.payload).context("invalid input epoch")?;
    gate.reset(epoch).await?;
    wire::write_frame(stream, wire::INPUT_RESET, &epoch.encode()).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::os::fd::FromRawFd;

    use super::*;

    #[tokio::test]
    async fn revoke_cancels_a_full_writer_and_discards_old_epoch_queue() {
        const TIMEOUT: Duration = Duration::from_secs(5);
        const OLD: &[u8] = b"OLD-EPOCH";
        const NEW: &[u8] = b"NEW-EPOCH";
        let mut fds = [0; 2];
        // SAFETY: fds has space for the descriptors; each becomes one owned File.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let reader = unsafe { File::from_raw_fd(fds[0]) };
        let mut writer = unsafe { File::from_raw_fd(fds[1]) };
        for fd in fds {
            // SAFETY: both descriptors remain open and owned by the Files above.
            assert_eq!(
                unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) },
                0
            );
        }
        let filler = [b'x'; 4096];
        loop {
            match writer.write(&filler) {
                Ok(written) => assert!(written > 0),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("could not fill the input pipe: {error}"),
            }
        }
        let old = InputEpoch {
            connection: 1,
            browser: 1,
        };
        let new = InputEpoch {
            connection: 1,
            browser: 2,
        };
        let (desired, current) = watch::channel(old);
        let (confirmed, mut applied) = watch::channel(old);
        let (input, queue) = mpsc::channel(1);
        let task = tokio::spawn(write_loop(
            AsyncFd::new(writer).unwrap(),
            queue,
            current,
            confirmed,
        ));
        input
            .send(Input {
                epoch: old,
                bytes: OLD.to_vec(),
            })
            .await
            .unwrap();
        // The second send completes only after the writer removes the first
        // frame. The full pipe blocks that frame while the second stays queued.
        tokio::time::timeout(
            TIMEOUT,
            input.send(Input {
                epoch: old,
                bytes: OLD.to_vec(),
            }),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(input.capacity(), 0);
        desired.send_replace(new);
        tokio::time::timeout(TIMEOUT, applied.wait_for(|epoch| *epoch == new))
            .await
            .unwrap()
            .unwrap();
        let mut reader = reader;
        let mut buffer = [0; 4096];
        loop {
            match reader.read(&mut buffer) {
                Ok(read) => {
                    assert!(read > 0);
                    assert!(
                        buffer[..read].iter().all(|byte| *byte == b'x'),
                        "old input crossed the reset acknowledgement"
                    );
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("could not drain the input pipe: {error}"),
            }
        }
        input
            .send(Input {
                epoch: new,
                bytes: NEW.to_vec(),
            })
            .await
            .unwrap();
        let reader = AsyncFd::new(reader).unwrap();
        let received = tokio::time::timeout(TIMEOUT, async {
            let mut received = Vec::new();
            while received.len() < NEW.len() {
                let mut ready = reader.readable().await.unwrap();
                match ready.try_io(|inner| inner.get_ref().read(&mut buffer)) {
                    Ok(Ok(read)) => {
                        assert!(read > 0);
                        received.extend_from_slice(&buffer[..read]);
                    }
                    Ok(Err(error)) => panic!("could not read the new epoch: {error}"),
                    Err(_) => continue,
                }
            }
            received
        })
        .await
        .unwrap();
        assert_eq!(received, NEW, "a queued old frame survived the epoch reset");
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
    }
}
