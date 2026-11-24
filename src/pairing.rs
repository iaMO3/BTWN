use crate::{discovery, lan::Pair, storage};
use anyhow::{Context, Result, ensure};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    net::{Ipv4Addr, UdpSocket},
    path::Path,
};

#[derive(Serialize, Deserialize)]
struct Invitation {
    format: String,
    connection: Pair,
}

fn stage(identity: &Path) -> Result<tempfile::TempDir> {
    ensure!(!identity.exists(), "This PC is already paired.");
    let parent = identity
        .parent()
        .context("Missing BTWN profile directory")?;
    fs::create_dir_all(parent)?;
    Ok(tempfile::Builder::new()
        .prefix("pair-")
        .tempdir_in(parent)?)
}

pub fn create(identity: &Path) -> Result<Pair> {
    let temp = stage(identity)?;
    let key = rcgen::generate_simple_self_signed(vec!["btwn.local".into()])?;
    let mut secret = [0u8; 32];
    SystemRandom::new()
        .fill(&mut secret)
        .map_err(|_| anyhow::anyhow!("Windows randomness unavailable"))?;
    // UDP connect selects a local route without sending a packet. Discovery also broadcasts.
    let address = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
        .and_then(|socket| {
            socket.connect((Ipv4Addr::new(239, 255, 0, 1), discovery::PORT))?;
            socket.local_addr()
        })
        .ok()
        .and_then(|addr| match addr.ip() {
            std::net::IpAddr::V4(ip) if ip.is_private() => Some(ip),
            _ => None,
        })
        .unwrap_or(Ipv4Addr::LOCALHOST);
    let pair = Pair {
        address,
        port: discovery::PORT,
        fingerprint: storage::checksum(key.cert.der().as_ref()),
        token: storage::checksum(&secret),
    };
    fs::write(temp.path().join("certificate.pem"), key.cert.pem())?;
    fs::write(
        temp.path().join("private.pem"),
        key.key_pair.serialize_pem(),
    )?;
    storage::write(&temp.path().join("pair.json"), &pair)?;
    fs::rename(temp.path(), identity).context("Activate host identity")?;
    Ok(pair)
}

pub fn export(pair: &Pair, file: &Path) -> Result<()> {
    let mut out =
        tempfile::NamedTempFile::new_in(file.parent().context("Missing invitation folder")?)?;
    out.write_all(&serde_json::to_vec_pretty(&Invitation {
        format: "btwn-pair-v1".into(),
        connection: pair.clone(),
    })?)?;
    out.as_file().sync_all()?;
    out.persist_noclobber(file)
        .context("Save pairing file using a new filename")?;
    Ok(())
}

pub fn import(identity: &Path, file: &Path) -> Result<()> {
    ensure!(
        fs::metadata(file)?.len() <= 8192,
        "Invalid BTWN pairing file"
    );
    let invitation: Invitation =
        storage::read(file).context("Open a pairing file created by BTWN 1.1 or later")?;
    ensure!(
        invitation.format == "btwn-pair-v1",
        "Unsupported BTWN pairing file"
    );
    let temp = stage(identity)?;
    let staged_pair = temp.path().join("pair.json");
    storage::write(&staged_pair, &invitation.connection)?;
    Pair::from_file(&staged_pair)?;
    fs::rename(temp.path(), identity).context("Activate peer identity")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_users_can_pair_without_replacing_existing_identity() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let host = dir.path().join("first-user/Identity");
        let peer = dir.path().join("second-user/Identity");
        let invite = dir.path().join("BTWN-pair.json");
        let pair = create(&host)?;
        export(&pair, &invite)?;
        import(&peer, &invite)?;
        let actual = Pair::from_file(&peer.join("pair.json"))?;
        assert_eq!(actual.fingerprint, pair.fingerprint);
        assert_eq!(actual.token, pair.token);
        assert!(!peer.join("private.pem").exists());
        assert!(create(&host).is_err());
        assert!(import(&peer, &invite).is_err());
        assert!(export(&pair, &invite).is_err());
        Ok(())
    }
}
