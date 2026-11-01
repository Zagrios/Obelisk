//! Whether a Mac build's game runs natively on Apple silicon, told from the
//! build's own files before it is installed. A storefront lists the files its
//! manifest names and reads the first bytes of the few asked for; nothing here
//! reaches the network or the disk. Obelisk installs a Mac build only when it
//! does: an Intel or 32-bit one goes through Wine as its Windows build.
use std::{
    collections::{BTreeMap, HashMap},
    future::Future,
};

/// How many bytes of a program tell its architectures: more than the header
/// of a universal binary of the most slices a launch accepts.
pub const PROGRAM_HEAD: u64 = 4096;
/// An Info.plist larger than this names no executable.
const MAX_INFO_PLIST: u64 = 1024 * 1024;
/// Links followed to reach one file.
const MAX_LINKS: usize = 8;
const CPU_TYPE_ARM64: u32 = 0x0100_000c;
/// A universal binary listing more slices is refused at launch.
const MAX_SLICES: u32 = 32;

enum Entry<F> {
    File {
        file: F,
        size: u64,
    },
    /// Its target, from the build's root.
    Link(String),
}

/// A build's files and links, by their path from the installation's root as
/// the storefront's manifest names them. Folders are those the paths go
/// through. Paths match whatever their case, as on the Mac's disk.
pub struct Listing<F> {
    entries: HashMap<String, (String, Entry<F>)>,
}

impl<F> Default for Listing<F> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }
}

impl<F: Copy> Listing<F> {
    /// A file, which `read` names as `file`. A later entry of the same path
    /// replaces it.
    pub fn file(&mut self, path: &str, file: F, size: u64) {
        self.insert(path, Entry::File { file, size });
    }

    /// A link to `target`, a path from the build's root.
    pub fn link(&mut self, path: &str, target: &str) {
        self.insert(path, Entry::Link(normalized(target)));
    }

    fn insert(&mut self, path: &str, entry: Entry<F>) {
        let path = normalized(path);
        self.entries.insert(path.to_lowercase(), (path, entry));
    }

    /// The file at `path`, through links, and its size.
    pub fn find(&self, path: &str) -> Option<(F, u64)> {
        let mut path = normalized(path);
        for _ in 0..=MAX_LINKS {
            match &self.entries.get(&path.to_lowercase())?.1 {
                Entry::File { file, size } => return Some((*file, *size)),
                Entry::Link(target) => path = target.clone(),
            }
        }
        None
    }

    fn is_folder(&self, path: &str) -> bool {
        let prefix = format!("{}/", normalized(path).to_lowercase());
        self.entries.keys().any(|key| key.starts_with(&prefix))
    }

    /// The first file under `folder` named like one of `names`, whatever
    /// their case, as a launch finds a library there: each folder's entries
    /// in name order, its files before its subfolders, the last subfolder
    /// first. Links and hidden entries are passed over.
    fn first_named(&self, folder: &str, names: &[&str]) -> Option<(F, u64)> {
        let folder = normalized(folder);
        let prefix = match folder.as_str() {
            "" => String::new(),
            folder => format!("{}/", folder.to_lowercase()),
        };
        // Each folder's entries, by its lowercased path: name, then the file
        // it is, or None for a subfolder.
        let mut folders: HashMap<String, BTreeMap<String, Option<(F, u64)>>> = HashMap::new();
        for (key, (path, entry)) in &self.entries {
            if !key.starts_with(&prefix) {
                continue;
            }
            let parts: Vec<&str> = path[prefix.len()..].split('/').collect();
            let mut parent = folder.to_lowercase();
            for (index, part) in parts.iter().enumerate() {
                let last = index + 1 == parts.len();
                let child = match last {
                    true => match entry {
                        Entry::File { file, size } => Some((*file, *size)),
                        Entry::Link(_) => break,
                    },
                    false => None,
                };
                folders
                    .entry(parent.clone())
                    .or_default()
                    .entry((*part).to_owned())
                    .or_insert(child);
                parent = match parent.as_str() {
                    "" => part.to_lowercase(),
                    parent => format!("{parent}/{}", part.to_lowercase()),
                };
            }
        }
        let mut pending = vec![folder.to_lowercase()];
        while let Some(current) = pending.pop() {
            for (name, child) in folders.get(&current).into_iter().flatten() {
                if name.starts_with('.') {
                    continue;
                }
                match child {
                    None => pending.push(match current.as_str() {
                        "" => name.to_lowercase(),
                        current => format!("{current}/{}", name.to_lowercase()),
                    }),
                    Some(file) if names.iter().any(|wanted| name.eq_ignore_ascii_case(wanted)) => {
                        return Some(*file);
                    }
                    Some(_) => {}
                }
            }
        }
        None
    }
}

/// Whether the game a Mac build starts from `executable`, a path from the
/// installation's root, runs natively on Apple silicon: its program holds an
/// arm64 slice, and so does the first library named like one of `libraries`
/// in the folder a launch searches, which must hold one. An application
/// bundle starts the executable its Info.plist names. `read` gives a file's
/// first bytes, never more than its size.
pub async fn runs_natively<F: Copy, E, Read>(
    listing: &Listing<F>,
    executable: &str,
    libraries: &[&str],
    read: impl Fn(F, u64) -> Read,
) -> Result<bool, E>
where
    Read: Future<Output = Result<Vec<u8>, E>>,
{
    let executable = normalized(executable);
    let bundle = executable.to_lowercase().ends_with(".app");
    let (program, searched) = if bundle {
        if !listing.is_folder(&executable) {
            return Ok(false);
        }
        let Some((plist, size)) = listing.find(&format!("{executable}/Contents/Info.plist")) else {
            return Ok(false);
        };
        if size > MAX_INFO_PLIST {
            return Ok(false);
        }
        let Some(name) = bundle_executable(&read(plist, size).await?) else {
            return Ok(false);
        };
        (
            format!("{executable}/Contents/MacOS/{name}"),
            executable.clone(),
        )
    } else {
        let parent = executable
            .rsplit_once('/')
            .map_or("", |(parent, _)| parent)
            .to_owned();
        (executable, parent)
    };
    let Some(program) = listing.find(&program) else {
        return Ok(false);
    };
    if !has_arm64(&read_head(program, &read).await?) {
        return Ok(false);
    }
    if libraries.is_empty() {
        return Ok(true);
    }
    match listing.first_named(&searched, libraries) {
        Some(library) => Ok(has_arm64(&read_head(library, &read).await?)),
        None => Ok(false),
    }
}

fn read_head<F, Read>((file, size): (F, u64), read: impl Fn(F, u64) -> Read) -> Read {
    read(file, size.min(PROGRAM_HEAD))
}

/// Whether a program starting with `head` holds an arm64 slice: a 64-bit
/// Mach-O for arm64, or a universal binary listing one.
pub fn has_arm64(head: &[u8]) -> bool {
    let word = |offset: usize, little: bool| -> Option<u32> {
        let bytes = head.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
        Some(match little {
            true => u32::from_le_bytes(bytes),
            false => u32::from_be_bytes(bytes),
        })
    };
    match head {
        [0xcf, 0xfa, 0xed, 0xfe, ..] => word(4, true) == Some(CPU_TYPE_ARM64),
        [0xfe, 0xed, 0xfa, 0xcf, ..] => word(4, false) == Some(CPU_TYPE_ARM64),
        [0xca, 0xfe, 0xba, magic @ (0xbe | 0xbf), ..]
        | [magic @ (0xbe | 0xbf), 0xba, 0xfe, 0xca, ..] => {
            let little = head[0] != 0xca;
            let entry = if *magic == 0xbf { 32 } else { 20 };
            // A Java class file starts the same way, with its version where
            // the slice count would be.
            word(4, little).is_some_and(|count| {
                (1..=MAX_SLICES).contains(&count)
                    && (0..count as usize)
                        .any(|index| word(8 + index * entry, little) == Some(CPU_TYPE_ARM64))
            })
        }
        _ => false,
    }
}

/// The executable an application bundle's Info.plist names, in XML or binary
/// form; none when it names no single file of `Contents/MacOS`.
pub fn bundle_executable(info_plist: &[u8]) -> Option<String> {
    const KEY: &str = "CFBundleExecutable";
    let name = match info_plist.starts_with(b"bplist00") {
        true => binary_string(info_plist, KEY),
        false => xml_string(info_plist, KEY),
    }?;
    (!name.is_empty() && name.len() <= 255 && !name.contains('/') && name != "." && name != "..")
        .then_some(name)
}

/// The string `key` names in an XML property list's dictionary.
fn xml_string(plist: &[u8], key: &str) -> Option<String> {
    let text = std::str::from_utf8(plist).ok()?;
    let marker = format!("<key>{key}</key>");
    let after = &text[text.find(&marker)? + marker.len()..];
    let value = after.trim_start().strip_prefix("<string>")?;
    unescape(&value[..value.find("</string>")?])
}

/// XML text with its entities replaced.
fn unescape(text: &str) -> Option<String> {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        output.push_str(&rest[..start]);
        let end = start + rest[start..].find(';')?;
        let entity = &rest[start + 1..end];
        let character = match entity {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                let code = entity.strip_prefix('#')?;
                let code = match code.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                    None => code.parse().ok()?,
                };
                char::from_u32(code)?
            }
        };
        output.push(character);
        rest = &rest[end + 1..];
    }
    output.push_str(rest);
    Some(output)
}

/// The string `key` names in a binary property list's top dictionary.
fn binary_string(plist: &[u8], key: &str) -> Option<String> {
    let trailer = plist.get(plist.len().checked_sub(32)?..)?;
    let (offset_size, reference_size) = (usize::from(trailer[6]), usize::from(trailer[7]));
    if !(1..=8).contains(&offset_size) || !(1..=8).contains(&reference_size) {
        return None;
    }
    let objects = number(&trailer[8..16])?;
    let top = number(&trailer[16..24])?;
    let table = usize::try_from(number(&trailer[24..32])?).ok()?;
    let object = |index: u64| -> Option<usize> {
        if index >= objects {
            return None;
        }
        let at = table.checked_add(usize::try_from(index).ok()?.checked_mul(offset_size)?)?;
        usize::try_from(number(plist.get(at..at.checked_add(offset_size)?)?)?).ok()
    };
    let start = object(top)?;
    if plist.get(start)? >> 4 != 0xd {
        return None;
    }
    let (entries, references) = length(plist, start)?;
    let reference = |index: usize| -> Option<u64> {
        let at = references.checked_add(index.checked_mul(reference_size)?)?;
        number(plist.get(at..at.checked_add(reference_size)?)?)
    };
    (0..entries).find_map(|index| {
        let name = string(plist, object(reference(index)?)?)?;
        (name == key)
            .then(|| string(plist, object(reference(entries.checked_add(index)?)?)?))
            .flatten()
    })
}

/// The length an object's marker at `start` gives, and where its content
/// starts.
fn length(plist: &[u8], start: usize) -> Option<(usize, usize)> {
    let info = plist.get(start)? & 0xf;
    if info != 0xf {
        return Some((usize::from(info), start + 1));
    }
    let marker = *plist.get(start + 1)?;
    if marker >> 4 != 0x1 || marker & 0xf > 3 {
        return None;
    }
    let size = 1usize << (marker & 0xf);
    let value = number(plist.get(start + 2..start + 2 + size)?)?;
    Some((usize::try_from(value).ok()?, start + 2 + size))
}

/// An ASCII or UTF-16 string object.
fn string(plist: &[u8], start: usize) -> Option<String> {
    let (count, content) = length(plist, start)?;
    match plist.get(start)? >> 4 {
        0x5 => {
            let bytes = plist.get(content..content.checked_add(count)?)?;
            bytes
                .is_ascii()
                .then(|| String::from_utf8_lossy(bytes).into_owned())
        }
        0x6 => {
            let bytes = plist.get(content..content.checked_add(count.checked_mul(2)?)?)?;
            let units: Vec<u16> = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|unit| u16::from_be_bytes(*unit))
                .collect();
            String::from_utf16(&units).ok()
        }
        _ => None,
    }
}

/// A big-endian unsigned number of at most eight bytes.
fn number(bytes: &[u8]) -> Option<u64> {
    (bytes.len() <= 8).then(|| {
        bytes
            .iter()
            .fold(0, |value, byte| value << 8 | u64::from(*byte))
    })
}

/// A path with forward slashes, without empty or `.` parts, its `..` parts
/// applied; a `..` past the root is dropped.
fn normalized(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    parts.join("/")
}

#[cfg(test)]
mod tests;
