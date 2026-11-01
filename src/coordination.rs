//! Small cross-process reservations for Flotilla fleets.
use std::path::{Path, PathBuf};

pub struct Reservation {
    path: PathBuf,
}

impl Reservation {
    pub fn acquire(path: impl AsRef<Path>, owner: &str) -> Result<Self, String> {
        let path = path.as_ref().to_owned();
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
        writeln!(file, "{owner}").map_err(|e| format!("reservation {}: {e}", path.display()))?;
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
}
