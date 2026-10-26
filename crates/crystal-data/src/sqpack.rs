//! SqPack: files by path from `<game>/sqpack/ffxiv/<category>.win32.index` and `.dat<n>`.
//!
//! A path is found by the JAMCRC of its folder and of its file name (lowercase). The index entry
//! names the dat file (bits 1-3) and the offset (the rest, times 8). A "standard" file there is a
//! header, then blocks: each a 0x10-byte header and raw-deflated data (stored when its compressed
//! size reads 32000).

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use flate2::read::DeflateDecoder;
use parking_lot::Mutex;

use crate::{DataError, Result, format};

/// The categories the server reads, by the path's first folder.
fn category(path: &str) -> Option<&'static str> {
    match path.split('/').next()? {
        "common" => Some("000000"),
        "bgcommon" => Some("010000"),
        "bg" => Some("020000"),
        "chara" => Some("040000"),
        "shader" => Some("050000"),
        "ui" => Some("060000"),
        "sound" => Some("070000"),
        "vfx" => Some("080000"),
        "exd" => Some("0a0000"),
        "game_script" => Some("0b0000"),
        "music" => Some("0c0000"),
        _ => None,
    }
}

fn jamcrc(s: &str) -> u32 {
    !crc32fast::hash(s.as_bytes())
}

/// (folder hash, file hash) → index value.
type Index = HashMap<(u32, u32), u32>;

pub struct SqPack {
    root: PathBuf,
    /// Each category's index, loaded on first use.
    indexes: Mutex<HashMap<&'static str, Index>>,
}

impl SqPack {
    /// `game` is the client's `game` folder (the one holding `sqpack/`).
    pub fn open(game: &Path) -> Result<Self> {
        let root = game.join("sqpack").join("ffxiv");
        if !root.is_dir() {
            return Err(DataError::NotFound(root.display().to_string()));
        }
        Ok(Self { root, indexes: Mutex::new(HashMap::new()) })
    }

    fn io(&self, path: &Path, source: std::io::Error) -> DataError {
        DataError::Io { path: path.display().to_string(), source }
    }

    fn lookup(&self, path: &str) -> Result<(&'static str, u32)> {
        let cat = category(path).ok_or_else(|| DataError::NotFound(path.to_owned()))?;
        let (folder, name) = path.rsplit_once('/').ok_or_else(|| DataError::NotFound(path.to_owned()))?;
        let mut indexes = self.indexes.lock();
        if !indexes.contains_key(cat) {
            let index = self.load_index(cat)?;
            indexes.insert(cat, index);
        }
        let value = indexes[cat].get(&(jamcrc(folder), jamcrc(name))).copied();
        value.map(|v| (cat, v)).ok_or_else(|| DataError::NotFound(path.to_owned()))
    }

    fn load_index(&self, cat: &str) -> Result<Index> {
        let path = self.root.join(format!("{cat}.win32.index"));
        let data = std::fs::read(&path).map_err(|e| self.io(&path, e))?;
        let u32_at = |at: usize| data.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
        let name = path.display().to_string();
        let header = u32_at(0x0c).ok_or_else(|| format(&name, "short header"))? as usize;
        let files = u32_at(header + 8).ok_or_else(|| format(&name, "short index header"))? as usize;
        let size = u32_at(header + 12).ok_or_else(|| format(&name, "short index header"))? as usize;
        let table = data.get(files..files + size).ok_or_else(|| format(&name, "index table out of range"))?;
        Ok(table
            .as_chunks::<16>()
            .0
            .iter()
            .map(|e| {
                let w = |i: usize| u32::from_le_bytes(e[i..i + 4].try_into().unwrap());
                ((w(4), w(0)), w(8))
            })
            .collect())
    }

    /// The file at `path` (lowercase, '/'-separated, e.g. `exd/territorytype.exh`).
    pub fn read(&self, path: &str) -> Result<Vec<u8>> {
        let path = path.to_ascii_lowercase();
        let (cat, value) = self.lookup(&path)?;
        let dat = self.root.join(format!("{cat}.win32.dat{}", (value >> 1) & 7));
        let offset = (value & !0xf) as u64 * 8;
        let mut f = File::open(&dat).map_err(|e| self.io(&dat, e))?;
        read_entry(&mut f, offset).map_err(|e| match e {
            DataError::Format { what, .. } => format(&path, what),
            DataError::Io { source, .. } => self.io(&dat, source),
            other => other,
        })
    }
}

fn read_entry(f: &mut File, offset: u64) -> Result<Vec<u8>> {
    let io = |source| DataError::Io { path: String::new(), source };
    let mut head = [0u8; 0x18];
    f.seek(SeekFrom::Start(offset)).map_err(io)?;
    f.read_exact(&mut head).map_err(io)?;
    let w = |b: &[u8], i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
    let (header_size, kind, size, blocks) = (w(&head, 0), w(&head, 4), w(&head, 8) as usize, w(&head, 0x14));
    if kind != 2 {
        return Err(format("", format!("entry type {kind} (only standard files are read)")));
    }
    let mut table = vec![0u8; blocks as usize * 8];
    f.read_exact(&mut table).map_err(io)?;
    let mut out = Vec::with_capacity(size);
    for b in table.as_chunks::<8>().0 {
        let block_offset = w(b, 0) as u64;
        f.seek(SeekFrom::Start(offset + header_size as u64 + block_offset)).map_err(io)?;
        let mut bh = [0u8; 0x10];
        f.read_exact(&mut bh).map_err(io)?;
        let (compressed, decompressed) = (w(&bh, 8) as usize, w(&bh, 12) as usize);
        if compressed == 32000 {
            let mut raw = vec![0u8; decompressed];
            f.read_exact(&mut raw).map_err(io)?;
            out.extend_from_slice(&raw);
        } else {
            let mut raw = vec![0u8; compressed];
            f.read_exact(&mut raw).map_err(io)?;
            DeflateDecoder::new(&raw[..]).read_to_end(&mut out).map_err(io)?;
        }
    }
    out.truncate(size);
    Ok(out)
}
