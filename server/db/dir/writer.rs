use std::io::Write as _;

use base::Error;
use bytes::{Buf, Bytes};
use nix::{fcntl::OFlag, sys::stat::Mode};
use tracing::info_span;

use crate::CompositeId;

use super::{IoCommand, Pool, Worker};

/// An open file for writing.
///
/// This can only be used from a worker thread, but ownership is passed back and forth.
pub struct WriteStream {
    file: Option<std::fs::File>,
    pool: Pool,
}

impl WriteStream {
    pub(crate) async fn write(&mut self, data: &mut Bytes) -> Result<(), Error> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pool.send(
            super::is_open,
            IoCommand::Write {
                span: info_span!("write"),
                file: self.file.take().expect("file should be open"),
                data: std::mem::take(data),
                tx,
            },
        )?;
        let (file, returned_data, r) = rx.await.expect("worker should respond");
        self.file = Some(file);
        *data = returned_data;
        let cnt = r?;
        data.advance(cnt);
        Ok(())
    }

    pub(crate) async fn sync_all(&mut self) -> Result<(), Error> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pool.send(
            super::is_open,
            IoCommand::SyncAll {
                span: info_span!("finish"),
                file: self.file.take().expect("file should be open"),
                tx,
            },
        )?;
        let (file, r) = rx.await.expect("worker should respond");
        self.file = Some(file);
        r.map_err(Into::into)
    }
}

impl Drop for WriteStream {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            self.pool
                .send(
                    |s| matches!(s, super::State::Open | super::State::Closing { .. }),
                    IoCommand::Abandon { file },
                )
                .expect("pool should not close while stream is open")
        }
    }
}

impl Pool {
    pub(crate) async fn create_file(
        &self,
        composite_id: CompositeId,
    ) -> Result<WriteStream, Error> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.send(
            super::is_open,
            super::IoCommand::CreateFile {
                span: info_span!("open_for_writing"),
                composite_id,
                tx,
            },
        )?;
        rx.await.expect("pool should not be closed")
    }
}

impl Worker {
    pub(super) fn create_file(
        &self,
        span: tracing::Span,
        composite_id: CompositeId,
        tx: tokio::sync::oneshot::Sender<Result<WriteStream, Error>>,
    ) {
        if tx.is_closed() {
            return;
        }
        let _enter = span.enter();
        let p = super::CompositeIdPath::from(composite_id);
        match crate::fs::openat(
            self.dir.0,
            &p,
            OFlag::O_WRONLY | OFlag::O_EXCL | OFlag::O_CREAT,
            Mode::S_IRUSR | Mode::S_IWUSR,
        ) {
            Err(e) => {
                let _ = tx.send(Err(e.into()));
            }
            Ok(file) => {
                {
                    let mut l = self.shared.inner.lock().expect("not poisoned");
                    l.write_streams += 1;
                }
                let _ = tx.send(Ok(WriteStream {
                    file: Some(file),
                    pool: super::Pool(self.shared.clone()),
                }));
            }
        }
    }

    pub(super) fn write(
        &self,
        span: tracing::Span,
        mut file: std::fs::File,
        data: Bytes,
        tx: tokio::sync::oneshot::Sender<(std::fs::File, Bytes, Result<usize, std::io::Error>)>,
    ) {
        if tx.is_closed() {
            self.dec_write_streams();
            return;
        }
        let _enter = span.enter();
        let r = file.write(&data);
        let _ = tx.send((file, data, r));
    }

    pub(super) fn sync_all(
        &self,
        span: tracing::Span,
        file: std::fs::File,
        tx: tokio::sync::oneshot::Sender<(std::fs::File, Result<(), std::io::Error>)>,
    ) {
        if tx.is_closed() {
            self.dec_write_streams();
            return;
        }
        let _enter = span.enter();
        let r = file.sync_all();
        let _ = tx.send((file, r));
    }

    #[inline(never)]
    fn dec_write_streams(&self) {
        let mut l = self.shared.inner.lock().expect("not poisoned");
        l.write_streams = l
            .write_streams
            .checked_sub(1)
            .expect("write_streams is balanced");
        if l.write_streams == 0 && matches!(l.state, super::State::Closing { .. }) {
            self.shared.worker_notify.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    #[tokio::test]
    async fn basic() {
        crate::testutil::init();
        let tmpdir = tempfile::Builder::new()
            .prefix("moonfire-db-test-reader")
            .tempdir()
            .unwrap();
        let one = const { std::num::NonZeroUsize::new(1).unwrap() };
        let pool = crate::dir::Pool::new(crate::dir::Config {
            path: tmpdir.path().to_owned(),
            db_uuid: Uuid::now_v7(),
            dir_uuid: Uuid::now_v7(),
            last_complete_open: None,
            current_open: Some(crate::db::Open {
                uuid: Uuid::now_v7(),
                id: 1,
            }),
        });
        pool.open(one).await.unwrap();
        pool.complete_open_for_write().await.unwrap();
        let mut f = pool
            .create_file(crate::CompositeId::new(1, 1))
            .await
            .unwrap();
        f.write(&mut bytes::Bytes::from_static(b"hello"))
            .await
            .unwrap();
        f.sync_all().await.unwrap();
    }
}
