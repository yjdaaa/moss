//! NV17-3 mock-only, bounded firmware download capability lifecycle.
//! Compiled ONLY under cfg(test). No HTTP route, OTA offer, device access,
//! real signed credential, installer, or firmware flash can use these tickets.
use super::{ArtifactStore, Entry, FakeGrant};
use getrandom::fill;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const MAX_TICKETS: usize = 16;
const MAX_SIMULTANEOUS_DOWNLOADS: usize = 2;
const MAX_TTL: Duration = Duration::from_secs(300);
const CHUNK: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TicketError {
    Denied,
    Busy,
    Expired,
    Corrupt,
}

struct Ticket {
    device: String,
    client: String,
    board: String,
    version: String,
    sha256: String,
    artifact_revision: u64,
    credential_revision: u64,
    deadline: Instant,
    alive: Arc<AtomicBool>,
}
#[derive(Default)]
struct Inner {
    pending: HashMap<[u8; 32], Ticket>,
    in_flight_devices: HashSet<String>,
}
#[derive(Clone, Default)]
struct Tickets(Arc<Mutex<Inner>>);

struct Download {
    tickets: Tickets,
    device: String,
    ticket: Ticket,
}
impl Drop for Download {
    fn drop(&mut self) {
        if let Ok(mut state) = self.tickets.0.lock() {
            state.in_flight_devices.remove(&self.device);
        }
    }
}
fn digest(value: &str) -> [u8; 32] {
    Sha256::digest(value.as_bytes()).into()
}
fn check_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl Tickets {
    /// The caller must be a trusted authenticated device registry. This
    /// fake fixture only imitates that boundary; self-asserted HTTP fields
    /// must NEVER call mint() in a future production implementation.
    fn mint(
        &self,
        store: &ArtifactStore,
        grant: &FakeGrant<'_>,
        wanted: &Entry,
        credential_revision: u64,
        ttl: Duration,
        alive: Arc<AtomicBool>,
    ) -> Result<String, TicketError> {
        if !grant.allowed
            || !check_id(grant.device)
            || !check_id(grant.client)
            || !alive.load(Ordering::Acquire)
            || credential_revision == 0
            || ttl.is_zero()
            || ttl > MAX_TTL
        {
            return Err(TicketError::Denied);
        }
        let current = store.offer_fake(grant).ok_or(TicketError::Denied)?;
        if current.revision != wanted.revision
            || current.hash != wanted.hash
            || current.filename != wanted.filename
            || !current.enabled
            || current.board != wanted.board
            || current.version != wanted.version
        {
            return Err(TicketError::Denied);
        }
        let mut state = self.0.lock().map_err(|_| TicketError::Denied)?;
        state
            .pending
            .retain(|_, t| t.deadline > Instant::now() && t.alive.load(Ordering::Acquire));
        if state.pending.len() >= MAX_TICKETS
            || state.pending.values().any(|t| t.device == grant.device)
        {
            return Err(TicketError::Busy);
        }
        let mut random = [0u8; 32];
        fill(&mut random).map_err(|_| TicketError::Denied)?;
        let token = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let key = digest(&token);
        if state.pending.contains_key(&key) {
            return Err(TicketError::Denied);
        }
        state.pending.insert(
            key,
            Ticket {
                device: grant.device.to_owned(),
                client: grant.client.to_owned(),
                board: current.board,
                version: current.version,
                sha256: current.hash,
                artifact_revision: current.revision,
                credential_revision,
                deadline: Instant::now() + ttl,
                alive,
            },
        );
        Ok(token)
    }

    /// Atomic single-use authorization and globally/per-device capped
    /// concurrent fixture downloads. Does not produce an HTTP response.
    fn claim(
        &self,
        token: &str,
        grant: &FakeGrant<'_>,
        current_revision: u64,
    ) -> Result<Download, TicketError> {
        if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(TicketError::Denied);
        }
        let mut state = self.0.lock().map_err(|_| TicketError::Denied)?;
        let ticket = state
            .pending
            .remove(&digest(token))
            .ok_or(TicketError::Denied)?;
        if ticket.deadline <= Instant::now() {
            return Err(TicketError::Expired);
        }
        if !ticket.alive.load(Ordering::Acquire)
            || !grant.allowed
            || ticket.device != grant.device
            || ticket.client != grant.client
            || ticket.board != grant.board
            || current_revision != ticket.credential_revision
        {
            return Err(TicketError::Denied);
        }
        if state.in_flight_devices.len() >= MAX_SIMULTANEOUS_DOWNLOADS
            || !state.in_flight_devices.insert(grant.device.to_owned())
        {
            return Err(TicketError::Busy);
        }
        Ok(Download {
            tickets: self.clone(),
            device: grant.device.into(),
            ticket,
        })
    }
}

impl Download {
    /// The fake receiver is a closure; it never opens a socket or sends to a
    /// device. Each chunk checks cancellation and expiry, then stops without
    /// marking the firmware installed. A delivered byte cannot be retracted.
    fn copy_to_fake_receiver(
        &self,
        store: &ArtifactStore,
        grant: &FakeGrant<'_>,
        mut receiver: impl FnMut(&[u8]) -> Result<(), TicketError>,
    ) -> Result<usize, TicketError> {
        self.ensure_current(grant)?;
        let record = store
            .records
            .get(&(self.ticket.board.clone(), self.ticket.version.clone()))
            .ok_or(TicketError::Denied)?;
        if record.hash != self.ticket.sha256
            || record.revision != self.ticket.artifact_revision
            || !record.enabled
        {
            return Err(TicketError::Denied);
        }
        let bytes = store
            .read_fake(record, grant)
            .map_err(|_| TicketError::Corrupt)?;
        let mut written = 0;
        for chunk in bytes.chunks(CHUNK) {
            self.ensure_current(grant)?;
            receiver(chunk)?;
            written += chunk.len();
        }
        self.ensure_current(grant)?;
        Ok(written)
    }
    fn ensure_current(&self, grant: &FakeGrant<'_>) -> Result<(), TicketError> {
        if self.ticket.deadline <= Instant::now() {
            return Err(TicketError::Expired);
        }
        if !self.ticket.alive.load(Ordering::Acquire)
            || !grant.allowed
            || self.ticket.device != grant.device
            || self.ticket.client != grant.client
            || self.ticket.board != grant.board
        {
            return Err(TicketError::Denied);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn store(board: &str, bytes: &[u8]) -> (tempfile::TempDir, ArtifactStore, Entry) {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = ArtifactStore::new(tmp.path()).unwrap();
        let first = store.import_fake(board, "2.0", bytes).unwrap();
        let enabled = store
            .confirm_fake(board, "2", &first.hash, first.revision)
            .unwrap();
        (tmp, store, enabled)
    }
    fn fake<'a>(device: &'a str, client: &'a str, board: &'a str) -> FakeGrant<'a> {
        FakeGrant {
            board,
            device,
            client,
            allowed: true,
            reported_version: "1",
        }
    }
    #[test]
    fn one_time_token_exact_device_client_board_and_revision() {
        let (_tmp, store, record) = store("BOARD-A", b"NV17_SAFE_FAKE");
        let pool = Tickets::default();
        let on = Arc::new(AtomicBool::new(true));
        let g = fake("device-a", "client-a", "BOARD-A");
        let token = pool
            .mint(&store, &g, &record, 5, Duration::from_secs(5), on)
            .unwrap();
        assert_eq!(token.len(), 64);
        let d = pool.claim(&token, &g, 5).unwrap();
        let mut output = Vec::new();
        assert_eq!(
            d.copy_to_fake_receiver(&store, &g, |chunk| {
                output.extend_from_slice(chunk);
                Ok(())
            })
            .unwrap(),
            14
        );
        assert_eq!(output, b"NV17_SAFE_FAKE");
        assert!(pool.claim(&token, &g, 5).is_err());
        drop(d);
        let wrong = pool
            .mint(
                &store,
                &g,
                &record,
                7,
                Duration::from_secs(5),
                Arc::new(AtomicBool::new(true)),
            )
            .unwrap();
        assert_eq!(
            pool.claim(&wrong, &fake("device-a", "wrong", "BOARD-A"), 7)
                .err(),
            Some(TicketError::Denied)
        );
        assert_eq!(pool.claim(&wrong, &g, 7).err(), Some(TicketError::Denied));
        let stale = pool
            .mint(
                &store,
                &g,
                &record,
                7,
                Duration::from_secs(5),
                Arc::new(AtomicBool::new(true)),
            )
            .unwrap();
        assert_eq!(pool.claim(&stale, &g, 8).err(), Some(TicketError::Denied));
    }
    #[test]
    fn revoked_disabled_tampered_and_expired_capabilities_fail_closed() {
        let (tmp, mut store, record) = store("B", b"NV17_NO_REAL_FW");
        let pool = Tickets::default();
        let g = fake("device", "client", "B");
        let on = Arc::new(AtomicBool::new(true));
        let token = pool
            .mint(&store, &g, &record, 1, Duration::from_secs(5), on.clone())
            .unwrap();
        on.store(false, Ordering::Release);
        assert_eq!(pool.claim(&token, &g, 1).err(), Some(TicketError::Denied));
        let token = pool
            .mint(
                &store,
                &g,
                &record,
                1,
                Duration::from_millis(12),
                Arc::new(AtomicBool::new(true)),
            )
            .unwrap();
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(pool.claim(&token, &g, 1).err(), Some(TicketError::Expired));

        let token = pool
            .mint(
                &store,
                &g,
                &record,
                1,
                Duration::from_secs(5),
                Arc::new(AtomicBool::new(true)),
            )
            .unwrap();
        store.disable_fake("B", "2");
        let d = pool.claim(&token, &g, 1).unwrap();
        assert_eq!(
            d.copy_to_fake_receiver(&store, &g, |_| Ok(())).err(),
            Some(TicketError::Denied)
        );
        drop(d);

        let new = store.confirm_fake("B", "2", &record.hash, 3).unwrap();
        let token = pool
            .mint(
                &store,
                &g,
                &new,
                1,
                Duration::from_secs(5),
                Arc::new(AtomicBool::new(true)),
            )
            .unwrap();
        std::fs::write(tmp.path().join(&new.filename), b"TAMPER").unwrap();
        let d = pool.claim(&token, &g, 1).unwrap();
        assert_eq!(
            d.copy_to_fake_receiver(&store, &g, |_| Ok(())).err(),
            Some(TicketError::Corrupt)
        );
    }
    #[test]
    fn per_device_and_global_two_downloads_refuse_third_without_replay() {
        let (_tmp, store, record) = store("B", b"NV17_FAKE");
        let pool = Tickets::default();
        let mut active = Vec::new();
        for n in 0..2 {
            let id = format!("device-{n}");
            let grant = fake(&id, "client", "B");
            let token = pool
                .mint(
                    &store,
                    &grant,
                    &record,
                    1,
                    Duration::from_secs(5),
                    Arc::new(AtomicBool::new(true)),
                )
                .unwrap();
            active.push(pool.claim(&token, &grant, 1).unwrap());
        }
        let third = fake("device-3", "client", "B");
        let token = pool
            .mint(
                &store,
                &third,
                &record,
                1,
                Duration::from_secs(5),
                Arc::new(AtomicBool::new(true)),
            )
            .unwrap();
        assert_eq!(pool.claim(&token, &third, 1).err(), Some(TicketError::Busy));
        drop(active.remove(0));
        let token = pool
            .mint(
                &store,
                &third,
                &record,
                1,
                Duration::from_secs(5),
                Arc::new(AtomicBool::new(true)),
            )
            .unwrap();
        let current = pool.claim(&token, &third, 1).unwrap();
        assert_eq!(
            pool.claim(&token, &third, 1).err(),
            Some(TicketError::Denied)
        );
        drop(current);
    }
    #[test]
    fn callback_cancellation_blocks_following_chunks_and_never_marks_flash_success() {
        let bytes = vec![b'F'; CHUNK * 3];
        let (_tmp, store, record) = store("B", &bytes);
        let pool = Tickets::default();
        let live = Arc::new(AtomicBool::new(true));
        let grant = fake("device", "client", "B");
        let token = pool
            .mint(
                &store,
                &grant,
                &record,
                1,
                Duration::from_secs(5),
                live.clone(),
            )
            .unwrap();
        let download = pool.claim(&token, &grant, 1).unwrap();
        let mut received = 0;
        assert_eq!(
            download
                .copy_to_fake_receiver(&store, &grant, |chunk| {
                    received += chunk.len();
                    live.store(false, Ordering::Release);
                    Ok(())
                })
                .err(),
            Some(TicketError::Denied)
        );
        assert_eq!(received, CHUNK);
        drop(download);
        assert_eq!(pool.0.lock().unwrap().in_flight_devices.len(), 0);
    }
}
