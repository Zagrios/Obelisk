//! Select supported Mach-O slices before launching native games or Steam SDK code.
use crate::Error;
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Architecture {
    Arm64,
    Intel,
}
impl Architecture {
    pub fn select(executable: &Path, library: &Path) -> Result<Self, Error> {
        Self::from_slices(slices(executable)? & slices(library)?)
    }
    /// The preferred supported slice of an executable or library on its own.
    pub fn preferred(binary: &Path) -> Result<Self, Error> {
        Self::from_slices(slices(binary)?)
    }
    fn from_slices(mask: u8) -> Result<Self, Error> {
        if mask & 1 != 0 {
            Ok(Self::Arm64)
        } else if mask & 2 != 0 {
            Ok(Self::Intel)
        } else {
            Err(Error::IncompatibleArchitecture)
        }
    }
}

fn cpu_mask(cpu: u32) -> u8 {
    match cpu {
        0x0100_000c => 1,
        0x0100_0007 => 2,
        _ => 0,
    }
}
fn number(bytes: &[u8], little: bool) -> u32 {
    let bytes = bytes.try_into().expect("four-byte Mach-O field");
    if little {
        u32::from_le_bytes(bytes)
    } else {
        u32::from_be_bytes(bytes)
    }
}
fn wide(bytes: &[u8], little: bool) -> u64 {
    let bytes = bytes.try_into().expect("eight-byte Mach-O field");
    if little {
        u64::from_le_bytes(bytes)
    } else {
        u64::from_be_bytes(bytes)
    }
}
fn header(file: &mut File, offset: u64) -> Result<(u32, bool), Error> {
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = [0; 8];
    file.read_exact(&mut bytes)
        .map_err(|_| Error::IncompatibleArchitecture)?;
    let little = match bytes[..4] {
        [0xcf, 0xfa, 0xed, 0xfe] | [0xce, 0xfa, 0xed, 0xfe] => true,
        [0xfe, 0xed, 0xfa, 0xcf] | [0xfe, 0xed, 0xfa, 0xce] => false,
        _ => return Err(Error::IncompatibleArchitecture),
    };
    Ok((
        number(&bytes[4..], little),
        bytes[0] == 0xcf || bytes[3] == 0xcf,
    ))
}
fn slices(path: &Path) -> Result<u8, Error> {
    let mut file = File::open(path).map_err(|_| Error::Unavailable)?;
    let length = file.metadata()?.len();
    let mut prefix = [0; 8];
    file.read_exact(&mut prefix)
        .map_err(|_| Error::IncompatibleArchitecture)?;
    let fat = match prefix[..4] {
        [0xca, 0xfe, 0xba, 0xbe] => Some((false, false)),
        [0xbe, 0xba, 0xfe, 0xca] => Some((true, false)),
        [0xca, 0xfe, 0xba, 0xbf] => Some((false, true)),
        [0xbf, 0xba, 0xfe, 0xca] => Some((true, true)),
        _ => None,
    };
    let Some((little, is_wide)) = fat else {
        let (cpu, is_64) = header(&mut file, 0)?;
        return if is_64 && length >= 32 && cpu_mask(cpu) != 0 {
            Ok(cpu_mask(cpu))
        } else {
            Err(Error::IncompatibleArchitecture)
        };
    };
    let count = number(&prefix[4..], little) as usize;
    if !(1..=32).contains(&count) {
        return Err(Error::IncompatibleArchitecture);
    }
    let entry_size = if is_wide { 32 } else { 20 };
    let table_end = 8 + count * entry_size;
    let mut entries = vec![0; count * entry_size];
    file.read_exact(&mut entries)
        .map_err(|_| Error::IncompatibleArchitecture)?;
    let mut result = 0;
    let mut intervals = Vec::new();
    for entry in entries.chunks_exact(entry_size) {
        let cpu = number(&entry[..4], little);
        let (offset, size) = if is_wide {
            (wide(&entry[8..16], little), wide(&entry[16..24], little))
        } else {
            (
                number(&entry[8..12], little) as u64,
                number(&entry[12..16], little) as u64,
            )
        };
        let end = offset
            .checked_add(size)
            .ok_or(Error::IncompatibleArchitecture)?;
        if offset < table_end as u64
            || size < 28
            || end > length
            || intervals
                .iter()
                .any(|&(start, stop)| offset < stop && end > start)
        {
            return Err(Error::IncompatibleArchitecture);
        }
        intervals.push((offset, end));
        let (actual, is_64) = header(&mut file, offset)?;
        if actual != cpu || cpu_mask(cpu) != 0 && (!is_64 || size < 32) {
            return Err(Error::IncompatibleArchitecture);
        }
        result |= cpu_mask(cpu);
    }
    if result == 0 {
        Err(Error::IncompatibleArchitecture)
    } else {
        Ok(result)
    }
}

/// A bounded execution of our signed Intel helper proves availability. It does
/// not open an app through LaunchServices or request a system installation.
pub(crate) fn rosetta_available(helper: &Path) -> Result<bool, crate::supervisor::Error> {
    use crate::supervisor::Error;
    let mut child = match Command::new(helper)
        .arg("--probe")
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(error) if error.raw_os_error() == Some(86) => return Ok(false),
        Err(_) => return Err(Error::Unavailable),
    };
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(true),
            Ok(Some(_)) => return Err(Error::RosettaUnavailable),
            Ok(None) if start.elapsed() < Duration::from_secs(3) => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::RosettaUnavailable);
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
