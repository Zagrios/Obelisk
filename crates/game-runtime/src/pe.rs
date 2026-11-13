//! What Obelisk reads of a Windows program, a PE file: the machine its code
//! is for, whether it is a .NET program, the DLLs it imports and the
//! description its version resource gives. Only the headers and the parts
//! they point to are read, each within bounds, so a game's largest program
//! reads as fast as its smallest; a file that is no PE, or whose headers
//! point outside it, reads as nothing.
use std::{fs::File, os::unix::fs::FileExt, path::Path};

/// The machine of x86-64 code.
const AMD64: u16 = 0x8664;
/// The data directories Obelisk reads, by index.
const IMPORTS: usize = 1;
const RESOURCES: usize = 2;
const DELAY_IMPORTS: usize = 13;
/// .NET's header: a program with one is a .NET program.
const CLR: usize = 14;
/// The resource type of a version resource.
const VERSION: u32 = 16;
/// At most this many sections, DLLs imported, resource entries a directory
/// lists, and bytes a DLL's name or a version resource holds.
const SECTIONS: usize = 96;
const DLLS: usize = 1024;
const ENTRIES: usize = 1024;
const NAME: usize = 256;
const VERSION_SIZE: u32 = 64 * 1024;

/// A Windows program, read from its file as each question is asked.
pub(crate) struct Program {
    file: File,
    machine: u16,
    /// Each data directory's address as the program loads, 0 for none.
    directories: Vec<u32>,
    sections: Vec<Section>,
}

/// Where a part of the loaded program is in its file.
struct Section {
    address: u32,
    size: u32,
    offset: u32,
    stored: u32,
}

impl Program {
    /// The program at `path`, when it is a Windows program.
    pub(crate) fn open(path: &Path) -> Option<Self> {
        let file = File::open(path).ok()?;
        let dos = read(&file, 0, 64)?;
        if &dos[..2] != b"MZ" {
            return None;
        }
        let header = u64::from(u32_at(&dos, 60)?);
        let coff = read(&file, header, 24)?;
        if &coff[..4] != b"PE\0\0" {
            return None;
        }
        let machine = u16_at(&coff, 4)?;
        let section_count = usize::from(u16_at(&coff, 6)?);
        let optional_size = usize::from(u16_at(&coff, 20)?);
        if section_count > SECTIONS {
            return None;
        }
        let optional = read(&file, header + 24, optional_size)?;
        // PE32 and PE32+ place the directories' count and the directories
        // apart by the width of their addresses.
        let (count_at, first) = match u16_at(&optional, 0)? {
            0x10b => (92, 96),
            0x20b => (108, 112),
            _ => return None,
        };
        let count = (u32_at(&optional, count_at)? as usize).min(16);
        let directories = (0..count)
            .map(|index| u32_at(&optional, first + index * 8))
            .collect::<Option<_>>()?;
        let table = read(
            &file,
            header + 24 + optional_size as u64,
            section_count * 40,
        )?;
        let sections = table
            .as_chunks::<40>()
            .0
            .iter()
            .map(|section| {
                Some(Section {
                    size: u32_at(section, 8)?,
                    address: u32_at(section, 12)?,
                    stored: u32_at(section, 16)?,
                    offset: u32_at(section, 20)?,
                })
            })
            .collect::<Option<_>>()?;
        Some(Self {
            file,
            machine,
            directories,
            sections,
        })
    }

    /// Whether its code is x86-64.
    pub(crate) fn is_x86_64(&self) -> bool {
        self.machine == AMD64
    }

    /// Whether it is a .NET program, which a .NET runtime starts.
    pub(crate) fn is_dotnet(&self) -> bool {
        self.directory(CLR).is_some()
    }

    /// The DLLs it imports, as it loads or as it first calls them, in
    /// lowercase.
    pub(crate) fn imports(&self) -> Vec<String> {
        let mut dlls = Vec::new();
        // Each descriptor's size, and where in it the DLL's name is.
        for (directory, size, name_at) in [(IMPORTS, 20, 12), (DELAY_IMPORTS, 32, 4)] {
            let Some(mut at) = self
                .directory(directory)
                .and_then(|address| self.offset(address))
            else {
                continue;
            };
            for _ in 0..DLLS {
                let Some(name) =
                    read(&self.file, at, size).and_then(|entry| u32_at(&entry, name_at))
                else {
                    break;
                };
                if name == 0 {
                    break;
                }
                dlls.extend(self.name(name).map(|name| name.to_ascii_lowercase()));
                at += size as u64;
            }
        }
        dlls
    }

    /// What its version resource describes it as, such as "Easy Anti-Cheat
    /// Bootstrapper (EOS)": the FileDescription of its first string table
    /// that gives one.
    pub(crate) fn description(&self) -> Option<String> {
        let version = self.version()?;
        let (_, info) = block(&version, 0)?;
        children(&version, info.children)
            .filter(|(key, _)| key == "StringFileInfo")
            .flat_map(|(_, file_info)| children(&version, file_info.children))
            .flat_map(|(_, table)| children(&version, table.children))
            .filter(|(key, _)| key == "FileDescription")
            .map(|(_, string)| text(&version, string.value))
            .find(|description| !description.is_empty())
    }

    /// Its version resource: the first one, in its first language.
    fn version(&self) -> Option<Vec<u8>> {
        let root = self.offset(self.directory(RESOURCES)?)?;
        let mut at = root;
        // A resource is three directories deep: by type, by name, by
        // language; the last one names its data.
        for depth in 0..3 {
            let header = read(&self.file, at, 16)?;
            let count = usize::from(u16_at(&header, 12)?) + usize::from(u16_at(&header, 14)?);
            let entries = read(&self.file, at + 16, count.min(ENTRIES) * 8)?;
            let target = entries
                .as_chunks::<8>()
                .0
                .iter()
                .find(|entry| depth > 0 || u32_at(*entry, 0) == Some(VERSION))
                .and_then(|entry| u32_at(entry, 4))?;
            let directory = target & 0x8000_0000 != 0;
            if directory != (depth < 2) {
                return None;
            }
            at = root + u64::from(target & 0x7fff_ffff);
        }
        let data = read(&self.file, at, 8)?;
        let size = u32_at(&data, 4)?;
        if size > VERSION_SIZE {
            return None;
        }
        read(&self.file, self.offset(u32_at(&data, 0)?)?, size as usize)
    }

    /// The address of data directory `index`, when the program has it.
    fn directory(&self, index: usize) -> Option<u32> {
        self.directories
            .get(index)
            .copied()
            .filter(|&address| address != 0)
    }

    /// Where `address` of the loaded program is in its file, when the file
    /// holds it.
    fn offset(&self, address: u32) -> Option<u64> {
        self.sections.iter().find_map(|section| {
            // A section without a loaded size loads what its file stores.
            let loaded = if section.size == 0 {
                section.stored
            } else {
                section.size
            };
            let within = address.checked_sub(section.address)?;
            (within < loaded.min(section.stored))
                .then(|| u64::from(section.offset) + u64::from(within))
        })
    }

    /// The ASCII name at `address`, such as a DLL's.
    fn name(&self, address: u32) -> Option<String> {
        let mut bytes = vec![0; NAME];
        let read = self.file.read_at(&mut bytes, self.offset(address)?).ok()?;
        let end = bytes[..read].iter().position(|&byte| byte == 0)?;
        String::from_utf8(bytes[..end].to_vec()).ok()
    }
}

/// One block of a version resource: where its value and its children are
/// in the resource.
struct Block {
    value: std::ops::Range<usize>,
    children: std::ops::Range<usize>,
}

/// The block at `at` in `version`, with its key.
fn block(version: &[u8], at: usize) -> Option<(String, Block)> {
    let end = at.checked_add(usize::from(u16_at(version, at)?))?;
    if end > version.len() || end < at + 6 {
        return None;
    }
    let value_length = usize::from(u16_at(version, at + 2)?);
    let is_text = u16_at(version, at + 4)? == 1;
    let key_start = at + 6;
    let key_end = (key_start..end)
        .step_by(2)
        .find(|&unit| u16_at(version, unit) == Some(0))?;
    let key = utf16(&version[key_start..key_end]);
    let value_start = aligned(key_end + 2).min(end);
    let value_end = if is_text {
        // Some programs count a text value in bytes rather than characters:
        // the text then ends at its NUL, whichever they count.
        value_start + value_length * 2
    } else {
        value_start + value_length
    }
    .min(end);
    Some((
        key,
        Block {
            value: value_start..value_end,
            children: aligned(value_end).min(end)..end,
        },
    ))
}

/// The blocks in `range` of `version`, each with its key.
fn children(
    version: &[u8],
    range: std::ops::Range<usize>,
) -> impl Iterator<Item = (String, Block)> + '_ {
    let mut at = range.start;
    std::iter::from_fn(move || {
        if at >= range.end {
            return None;
        }
        let child = block(&version[..range.end], at)?;
        at = aligned(child.1.children.end);
        Some(child)
    })
}

/// The text in `range` of `version`.
fn text(version: &[u8], range: std::ops::Range<usize>) -> String {
    utf16(&version[range]).trim().to_owned()
}

/// The UTF-16 text `bytes` hold, up to its NUL.
fn utf16(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|unit| u16::from_le_bytes(*unit))
        .take_while(|&unit| unit != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

/// `at` rounded up to a multiple of four, as a version resource aligns its
/// parts.
fn aligned(at: usize) -> usize {
    at.next_multiple_of(4)
}

/// `length` bytes of `file` from `offset`, when it holds them all.
fn read(file: &File, offset: u64, length: usize) -> Option<Vec<u8>> {
    let mut bytes = vec![0; length];
    file.read_exact_at(&mut bytes, offset).ok()?;
    Some(bytes)
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(at..at.checked_add(2)?)?.try_into().ok()?,
    ))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

#[cfg(test)]
pub(crate) mod tests;
