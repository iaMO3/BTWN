use crate::lan::{Pair, Status};
use anyhow::{Context, Result, ensure};
use ring::hmac;
use std::{
    net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

pub const PORT: u16 = 53328;
pub const DEFAULT_TIMEOUT_MS: u64 = 1300;
const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 167);
const PREFIX: &[u8] = b"Between-discovery-1";
const PAYLOAD: usize = 36;
const SIZE: usize = PAYLOAD + 32;

fn message(pair: &Pair, query: bool, challenge: &[u8; 16]) -> [u8; SIZE] {
    let mut packet = [0; SIZE];
    packet[..PREFIX.len()].copy_from_slice(PREFIX);
    packet[19] = u8::from(query);
    packet[20..36].copy_from_slice(challenge);
    let signature = hmac::sign(
        &hmac::Key::new(hmac::HMAC_SHA256, pair.token.as_bytes()),
        &packet[..PAYLOAD],
    );
    packet[PAYLOAD..].copy_from_slice(signature.as_ref());
    packet
}

fn verified(pair: &Pair, packet: &[u8], query: bool) -> bool {
    packet.len() == SIZE
        && packet.starts_with(PREFIX)
        && packet[19] == u8::from(query)
        && hmac::verify(
            &hmac::Key::new(hmac::HMAC_SHA256, pair.token.as_bytes()),
            &packet[..PAYLOAD],
            &packet[PAYLOAD..],
        )
        .is_ok()
}

pub fn start(pair: Pair, seen: Arc<AtomicU64>) -> Result<()> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, pair.port))
        .context("Start nearby-device discovery")?;
    // Broadcast and direct replies work on networks that filter multicast.
    let _ = socket.join_multicast_v4(&GROUP, &Ipv4Addr::UNSPECIFIED);
    std::thread::spawn(move || {
        let mut buffer = [0; SIZE + 1];
        while let Ok((size, from)) = socket.recv_from(&mut buffer) {
            if !crate::lan::private(from.ip()) || !verified(&pair, &buffer[..size], true) {
                continue;
            }
            let challenge = buffer[20..36]
                .try_into()
                .expect("verified discovery packet");
            seen.store(Status::now(), Ordering::Relaxed);
            let _ = socket.send_to(&message(&pair, false, &challenge), from);
        }
    });
    Ok(())
}

pub fn find(pair: &Pair, timeout_ms: u64) -> Result<Ipv4Addr> {
    let targets = [
        SocketAddrV4::new(GROUP, PORT),
        SocketAddrV4::new(Ipv4Addr::BROADCAST, PORT),
        SocketAddrV4::new(pair.address, PORT),
    ];
    find_at(
        pair,
        &targets,
        Duration::from_millis(timeout_ms.clamp(500, 5000)),
    )
}

fn find_at(pair: &Pair, targets: &[SocketAddrV4], wait: Duration) -> Result<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_broadcast(true)?;
    socket.set_multicast_ttl_v4(1)?;
    let challenge = *uuid::Uuid::new_v4().as_bytes();
    let packet = message(pair, true, &challenge);
    let mut sent = false;
    for target in targets {
        sent |= socket.send_to(&packet, target).is_ok();
    }
    ensure!(sent, "Could not send local network discovery");
    let timeout = Instant::now() + wait;
    let mut buffer = [0; SIZE + 1];
    while let Some(left) = timeout
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
    {
        socket.set_read_timeout(Some(left))?;
        let Ok((size, from)) = socket.recv_from(&mut buffer) else {
            break;
        };
        if let SocketAddr::V4(from) = from {
            if crate::lan::private((*from.ip()).into())
                && verified(pair, &buffer[..size], false)
                && buffer[20..36] == challenge
            {
                return Ok(*from.ip());
            }
        }
    }
    anyhow::bail!(
        "Hosting PC not found. Open BTWN on the PC that created the pairing file and check its private-network firewall rule."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovers_current_address_and_checks_pairing() -> Result<()> {
        let reservation = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = reservation.local_addr()?.port();
        drop(reservation);
        let pair = Pair {
            address: Ipv4Addr::new(192, 168, 1, 222),
            port,
            token: "1".repeat(64),
            fingerprint: "2".repeat(64),
        };
        let seen = Arc::new(AtomicU64::new(0));
        start(pair.clone(), seen.clone())?;
        let targets = [SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)];
        let wait = Duration::from_secs(1);
        assert_eq!(find_at(&pair, &targets, wait)?, Ipv4Addr::LOCALHOST);
        assert_ne!(seen.load(Ordering::Relaxed), 0);
        let wrong = Pair {
            token: "3".repeat(64),
            ..pair
        };
        assert!(find_at(&wrong, &targets, wait).is_err());
        Ok(())
    }
}
