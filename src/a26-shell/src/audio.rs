use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

pub struct AudioVolume {
    path: PathBuf,
}

impl AudioVolume {
    /// Audio may start after Moon (it is never waited for at boot), so a
    /// missing file is fine: Moon writes its volume there and the audio
    /// daemon picks it up when it starts.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self { path }),
            Err(error) => return Err(error),
        };
        if !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "audio volume path is not a regular file",
            ));
        }
        Ok(Self { path })
    }

    pub fn get(&self) -> io::Result<u8> {
        parse_volume(&fs::read_to_string(&self.path)?)
    }

    /// The user's volume at startup. A writer that rewrote the file in place
    /// could be caught between truncate and write; retry that briefly rather
    /// than falling back to a default and overwriting the user's value.
    pub fn startup_value(&self) -> io::Result<u8> {
        let mut result = self.get();
        for _ in 0..5 {
            match &result {
                Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                    thread::sleep(Duration::from_millis(20));
                    result = self.get();
                }
                _ => break,
            }
        }
        result
    }

    /// Atomic (write a sibling, then rename) so readers never see a partial
    /// value; keeps the file's audio-group ownership and mode.
    pub fn set(&self, volume: u8) -> io::Result<()> {
        let temporary = self.path.with_extension("new");
        {
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o660)
                .open(&temporary)?;
            file.write_all(format!("{}\n", volume.min(100)).as_bytes())?;
        }
        let _ = std::os::unix::fs::chown(&temporary, Some(1000), Some(1000));
        fs::rename(&temporary, &self.path)
    }
}

fn parse_volume(value: &str) -> io::Result<u8> {
    let volume = value
        .trim()
        .parse::<u8>()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid audio volume value"))?;
    if volume > 100 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "audio volume exceeds 100",
        ));
    }
    Ok(volume)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_is_atomic_and_startup_reads_the_written_value() {
        let dir = std::env::temp_dir().join(format!("moon-volume-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("volume");
        let missing = AudioVolume::open(&path).unwrap();
        assert_eq!(
            missing.startup_value().unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        missing.set(37).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "37\n");
        assert!(!dir.join("volume.new").exists());
        assert_eq!(
            AudioVolume::open(&path).unwrap().startup_value().unwrap(),
            37
        );
        // A half-written (empty) file is retried, then reported, not guessed.
        fs::write(&path, "").unwrap();
        assert_eq!(
            missing.startup_value().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn persisted_volume_accepts_mute_and_rejects_invalid_values() {
        assert_eq!(parse_volume("0\n").unwrap(), 0);
        assert_eq!(parse_volume("100").unwrap(), 100);
        assert!(parse_volume("101").is_err());
        assert!(parse_volume("not-a-volume").is_err());
    }
}
