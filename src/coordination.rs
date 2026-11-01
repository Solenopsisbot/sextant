//! Small cross-process reservations for Flotilla fleets.
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct Reservation {
    path: PathBuf,
}

impl Reservation {
    pub fn acquire(path: impl AsRef<Path>, owner: &str) -> Result<Self, String> {
        Self::acquire_with_ttl(path, owner, 24 * 60 * 60)
    }

    pub fn acquire_with_ttl(
        path: impl AsRef<Path>,
        owner: &str,
        ttl_seconds: u64,
    ) -> Result<Self, String> {
        let path = path.as_ref().to_owned();
        if let Ok(text) = std::fs::read_to_string(&path)
            && let Some(created) = text
                .split("\"created\":")
                .nth(1)
                .and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next())
                .and_then(|s| s.parse::<u64>().ok())
        {
            let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
            if now.saturating_sub(created) > ttl_seconds {
                let _ = std::fs::remove_file(&path);
            }
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| {
                format!(
                    "reservation {} is already held or unavailable: {e}",
                    path.display()
                )
            })?;
        use std::io::Write;
        let created = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        writeln!(
            file,
            "{{\"owner\":{},\"created\":{created}}}",
            serde_json::to_string(owner).unwrap_or_else(|_| "\"unknown\"".into())
        )
        .map_err(|e| format!("reservation {}: {e}", path.display()))?;
        Ok(Self { path })
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_one_owner_can_hold_a_reservation() {
        let path = std::env::temp_dir().join(format!("sextant-reservation-{}", std::process::id()));
        let first = Reservation::acquire(&path, "one").unwrap();
        assert!(Reservation::acquire(&path, "two").is_err());
        drop(first);
        assert!(Reservation::acquire(&path, "two").is_ok());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn stale_reservations_can_be_reclaimed() {
        let path = std::env::temp_dir().join(format!("sextant-stale-{}", std::process::id()));
        std::fs::write(&path, r#"{"owner":"old","created":1}"#).unwrap();
        let _owner = Reservation::acquire_with_ttl(&path, "new", 1).unwrap();
    }
}
