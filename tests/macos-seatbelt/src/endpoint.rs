use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub struct Endpoint {
    address: SocketAddr,
    _reservation: TcpListener,
    revoked: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
    worker: Option<JoinHandle<io::Result<()>>>,
}

impl Endpoint {
    pub fn start(reply: &'static [u8]) -> io::Result<Self> {
        Self::serve(TcpListener::bind("127.0.0.1:0")?, reply)
    }

    pub fn activate(reply: &'static [u8]) -> io::Result<Self> {
        Self::serve(crate::engine::relay::activate(c"Broker")?, reply)
    }

    fn serve(listener: TcpListener, reply: &'static [u8]) -> io::Result<Self> {
        let address = listener.local_addr()?;
        listener.set_nonblocking(true)?;
        // A failed serving thread must not release the port granted to live tools.
        let reservation = listener.try_clone()?;
        let revoked = Arc::new(AtomicBool::new(false));
        let stopped = Arc::new(AtomicBool::new(false));
        let revoke = Arc::clone(&revoked);
        let stop = Arc::clone(&stopped);
        let worker = thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                stream.set_read_timeout(Some(Duration::from_millis(500)))?;
                stream.set_write_timeout(Some(Duration::from_millis(500)))?;
                let mut request = [0; 4];
                if stream.read_exact(&mut request).is_err() || request != *b"ping" {
                    continue;
                }
                // Let the client close first so TIME_WAIT cannot fake a retained lease.
                if !matches!(stream.read(&mut [0]), Ok(0)) {
                    continue;
                }
                let response = if revoke.load(Ordering::SeqCst) {
                    b"revoked\n"
                } else {
                    reply
                };
                let _ = stream.write_all(response);
            }
            Ok(())
        });
        Ok(Self {
            address,
            _reservation: reservation,
            revoked,
            stopped,
            worker: Some(worker),
        })
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }

    pub fn revoke(&self) {
        self.revoked.store(true, Ordering::SeqCst);
    }

    pub fn stop_serving(&mut self) -> io::Result<()> {
        self.stopped.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| io::Error::other("endpoint worker panicked"))??;
        }
        Ok(())
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        self.stop_serving().unwrap();
    }
}
