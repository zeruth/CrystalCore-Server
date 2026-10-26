//! EXD sheets: `exd/<sheet>.exh` (columns, pages, languages) and `exd/<sheet>_<page>[_lang].exd`
//! (rows). Both are big-endian.

use crate::sqpack::SqPack;
use crate::{Result, format};

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    Bool(bool),
    Int(i64),
    UInt(u64),
    Float(f32),
}

impl Value {
    pub fn as_u64(&self) -> Option<u64> {
        match *self {
            Value::UInt(v) => Some(v),
            Value::Int(v) if v >= 0 => Some(v as u64),
            Value::Bool(b) => Some(b as u64),
            _ => None,
        }
    }

    pub fn as_f32(&self) -> Option<f32> {
        match *self {
            Value::Float(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }
}

/// One language of a sheet, all rows read.
#[derive(Debug, Clone)]
pub struct Sheet {
    pub name: String,
    pub rows: Vec<(u32, Vec<Value>)>,
}

impl Sheet {
    pub fn row(&self, id: u32) -> Option<&[Value]> {
        let i = self.rows.binary_search_by_key(&id, |r| r.0).ok()?;
        Some(&self.rows[i].1)
    }
}

const LANG_SUFFIX: [&str; 5] = ["", "_ja", "_en", "_de", "_fr"];

impl SqPack {
    /// A sheet in English where it has languages (the language-less data otherwise).
    pub fn sheet(&self, name: &str) -> Result<Sheet> {
        let exh_path = format!("exd/{name}.exh");
        let exh = self.read(&exh_path)?;
        if exh.get(..4) != Some(b"EXHF") {
            return Err(format(&exh_path, "not an EXH"));
        }
        let be16 = |d: &[u8], i: usize| u16::from_be_bytes(d[i..i + 2].try_into().unwrap());
        let be32 = |d: &[u8], i: usize| u32::from_be_bytes(d[i..i + 4].try_into().unwrap());
        let (row_size, columns, pages, langs) =
            (be16(&exh, 6) as usize, be16(&exh, 8) as usize, be16(&exh, 10) as usize, be16(&exh, 12) as usize);
        let cols: Vec<(u16, usize)> =
            (0..columns).map(|i| (be16(&exh, 0x20 + 4 * i), be16(&exh, 0x22 + 4 * i) as usize)).collect();
        let pages_at = 0x20 + 4 * columns;
        let page_starts: Vec<u32> = (0..pages).map(|i| be32(&exh, pages_at + 8 * i)).collect();
        let langs_at = pages_at + 8 * pages;
        let lang_codes: Vec<u16> = (0..langs)
            .map(|i| u16::from_le_bytes(exh[langs_at + 2 * i..langs_at + 2 * i + 2].try_into().unwrap()))
            .collect();
        let lang = if lang_codes.contains(&2) { 2 } else { lang_codes.first().copied().unwrap_or(0) } as usize;
        let suffix = LANG_SUFFIX.get(lang).copied().unwrap_or("");

        let mut rows = Vec::new();
        for start in page_starts {
            let path = format!("exd/{name}_{start}{suffix}.exd");
            let d = self.read(&path)?;
            if d.get(..4) != Some(b"EXDF") {
                return Err(format(&path, "not an EXD"));
            }
            let index_size = be32(&d, 8) as usize;
            for e in d[0x20..0x20 + index_size].chunks_exact(8) {
                let (id, at) = (be32(e, 0), be32(e, 4) as usize);
                let base = at + 6;
                let row =
                    d.get(base..base + row_size).ok_or_else(|| format(&path, format!("row {id} out of range")))?;
                let mut values = Vec::with_capacity(cols.len());
                for &(t, o) in &cols {
                    values.push(match t {
                        0 => {
                            let s = base + row_size + be32(row, o) as usize;
                            let end = d[s..].iter().position(|&b| b == 0).map_or(d.len(), |p| s + p);
                            Value::Str(String::from_utf8_lossy(&d[s..end]).into_owned())
                        }
                        1 => Value::Bool(row[o] != 0),
                        2 => Value::Int(row[o] as i8 as i64),
                        3 => Value::UInt(row[o] as u64),
                        4 => Value::Int(be16(row, o) as i16 as i64),
                        5 => Value::UInt(be16(row, o) as u64),
                        6 => Value::Int(be32(row, o) as i32 as i64),
                        7 => Value::UInt(be32(row, o) as u64),
                        9 => Value::Float(f32::from_bits(be32(row, o))),
                        0xb => Value::UInt(u64::from_be_bytes(row[o..o + 8].try_into().unwrap())),
                        0x19..=0x20 => Value::Bool(row[o] & (1 << (t - 0x19)) != 0),
                        _ => Value::UInt(0),
                    });
                }
                rows.push((id, values));
            }
        }
        rows.sort_by_key(|r| r.0);
        Ok(Sheet { name: name.to_owned(), rows })
    }
}
