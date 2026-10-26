use std::fs;
use std::io;
use std::path::{Path, PathBuf};

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

    pub fn set(&self, volume: u8) -> io::Result<()> {
        fs::write(&self.path, format!("{}\n", volume.min(100)))
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
    fn persisted_volume_accepts_mute_and_rejects_invalid_values() {
        assert_eq!(parse_volume("0\n").unwrap(), 0);
        assert_eq!(parse_volume("100").unwrap(), 100);
        assert!(parse_volume("101").is_err());
        assert!(parse_volume("not-a-volume").is_err());
    }
}
