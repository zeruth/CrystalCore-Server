//! EXD sheets: `exd/root.exl` (the sheet list), `exd/<sheet>.exh` (columns, pages, languages) and
//! `exd/<sheet>_<page>[_lang].exd` (rows). EXH and EXD are big-endian.

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

    pub fn as_i64(&self) -> Option<i64> {
        match *self {
            Value::Int(v) => Some(v),
            Value::UInt(v) => i64::try_from(v).ok(),
            Value::Bool(b) => Some(b as i64),
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

    /// `text` read as a value of `kind`'s type (for overrides, which are stored as text).
    pub fn parse(kind: ColumnKind, text: &str) -> Option<Value> {
        let t = text.trim();
        Some(match kind {
            ColumnKind::Str => Value::Str(text.to_owned()),
            ColumnKind::Bool => Value::Bool(match t.to_ascii_lowercase().as_str() {
                "1" | "true" => true,
                "0" | "false" => false,
                _ => return None,
            }),
            ColumnKind::Int => Value::Int(t.parse().ok()?),
            ColumnKind::UInt => Value::UInt(t.parse().ok()?),
            ColumnKind::Float => Value::Float(t.parse().ok()?),
        })
    }

    /// The zero of `kind` (a new row's columns before overrides fill them).
    pub fn zero(kind: ColumnKind) -> Value {
        match kind {
            ColumnKind::Str => Value::Str(String::new()),
            ColumnKind::Bool => Value::Bool(false),
            ColumnKind::Int => Value::Int(0),
            ColumnKind::UInt => Value::UInt(0),
            ColumnKind::Float => Value::Float(0.0),
        }
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Str(s) => f.write_str(s),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Int(v) => write!(f, "{v}"),
            Value::UInt(v) => write!(f, "{v}"),
            Value::Float(v) => write!(f, "{v}"),
        }
    }
}

/// What a column holds, as [`Value`] carries it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnKind {
    Str,
    Bool,
    Int,
    UInt,
    Float,
}

/// A column as the EXH declares it: its type code and its offset in the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Column {
    pub code: u16,
    pub offset: u16,
}

impl Column {
    pub fn kind(&self) -> ColumnKind {
        match self.code {
            0 => ColumnKind::Str,
            1 | 0x19..=0x20 => ColumnKind::Bool,
            2 | 4 | 6 => ColumnKind::Int,
            9 => ColumnKind::Float,
            _ => ColumnKind::UInt,
        }
    }

    /// The type's name, as `exd.py` prints it.
    pub fn type_name(&self) -> String {
        match self.code {
            0 => "str".into(),
            1 => "bool".into(),
            2 => "i8".into(),
            3 => "u8".into(),
            4 => "i16".into(),
            5 => "u16".into(),
            6 => "i32".into(),
            7 => "u32".into(),
            9 => "f32".into(),
            0xb => "u64".into(),
            c @ 0x19..=0x20 => format!("bit{}", c - 0x19),
            c => format!("?{c:#x}"),
        }
    }
}

/// Languages, by the EXH's language codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    None,
    Japanese,
    English,
    German,
    French,
    ChineseSimplified,
}

impl Language {
    fn suffix(self) -> &'static str {
        match self {
            Language::None => "",
            Language::Japanese => "_ja",
            Language::English => "_en",
            Language::German => "_de",
            Language::French => "_fr",
            Language::ChineseSimplified => "_chs",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "none" | "" => Language::None,
            "ja" => Language::Japanese,
            "en" => Language::English,
            "de" => Language::German,
            "fr" => Language::French,
            "chs" => Language::ChineseSimplified,
            _ => return None,
        })
    }

    fn from_code(code: u16) -> Option<Self> {
        Some(match code {
            0 => Language::None,
            1 => Language::Japanese,
            2 => Language::English,
            3 => Language::German,
            4 => Language::French,
            5 => Language::ChineseSimplified,
            _ => return None,
        })
    }
}

/// A sheet's EXH: its columns, pages (first row id, row count) and languages.
#[derive(Debug, Clone)]
pub struct Header {
    pub name: String,
    pub row_size: usize,
    pub columns: Vec<Column>,
    pub pages: Vec<(u32, u32)>,
    pub languages: Vec<Language>,
}

impl Header {
    /// English where the sheet has languages, its language-less data otherwise.
    pub fn default_language(&self) -> Language {
        if self.languages.contains(&Language::English) {
            Language::English
        } else {
            self.languages.first().copied().unwrap_or(Language::None)
        }
    }

    pub fn rows(&self) -> u32 {
        self.pages.iter().map(|p| p.1).sum()
    }
}

/// One language of a sheet, all rows read.
#[derive(Debug, Clone)]
pub struct Sheet {
    pub name: String,
    pub columns: Vec<Column>,
    pub rows: Vec<(u32, Vec<Value>)>,
}

impl Sheet {
    pub fn row(&self, id: u32) -> Option<&[Value]> {
        let i = self.rows.binary_search_by_key(&id, |r| r.0).ok()?;
        Some(&self.rows[i].1)
    }

    /// Sets column `col` of row `id` (made, with zeros, if the sheet has no such row). False when
    /// the sheet has no such column or `text` is not of its type.
    pub fn set(&mut self, id: u32, col: usize, text: &str) -> bool {
        let Some(kind) = self.columns.get(col).map(|c| c.kind()) else { return false };
        let Some(value) = Value::parse(kind, text) else { return false };
        let i = match self.rows.binary_search_by_key(&id, |r| r.0) {
            Ok(i) => i,
            Err(i) => {
                let zeros = self.columns.iter().map(|c| Value::zero(c.kind())).collect();
                self.rows.insert(i, (id, zeros));
                i
            }
        };
        self.rows[i].1[col] = value;
        true
    }
}

fn be16(d: &[u8], i: usize) -> u16 {
    u16::from_be_bytes(d[i..i + 2].try_into().unwrap())
}

fn be32(d: &[u8], i: usize) -> u32 {
    u32::from_be_bytes(d[i..i + 4].try_into().unwrap())
}

impl SqPack {
    /// Every sheet the client lists (`exd/root.exl`: `EXLT,2` then `Name,id` lines).
    pub fn sheet_names(&self) -> Result<Vec<String>> {
        let exl = self.read("exd/root.exl")?;
        let text = String::from_utf8_lossy(&exl);
        let mut names: Vec<String> = text
            .lines()
            .skip(1)
            .filter_map(|l| l.split(',').next())
            .map(|n| n.trim().to_owned())
            .filter(|n| !n.is_empty())
            .collect();
        names.sort();
        Ok(names)
    }

    pub fn header(&self, name: &str) -> Result<Header> {
        let path = format!("exd/{name}.exh");
        let exh = self.read(&path)?;
        if exh.get(..4) != Some(b"EXHF") || exh.len() < 0x20 {
            return Err(format(&path, "not an EXH"));
        }
        let (row_size, columns, pages, langs) =
            (be16(&exh, 6) as usize, be16(&exh, 8) as usize, be16(&exh, 10) as usize, be16(&exh, 12) as usize);
        let need = 0x20 + 4 * columns + 8 * pages + 2 * langs;
        if exh.len() < need {
            return Err(format(&path, "truncated EXH"));
        }
        let cols =
            (0..columns).map(|i| Column { code: be16(&exh, 0x20 + 4 * i), offset: be16(&exh, 0x22 + 4 * i) }).collect();
        let pages_at = 0x20 + 4 * columns;
        let page_list = (0..pages).map(|i| (be32(&exh, pages_at + 8 * i), be32(&exh, pages_at + 8 * i + 4))).collect();
        let langs_at = pages_at + 8 * pages;
        let languages = (0..langs)
            .filter_map(|i| Language::from_code(u16::from_le_bytes([exh[langs_at + 2 * i], exh[langs_at + 2 * i + 1]])))
            .collect();
        Ok(Header { name: name.to_owned(), row_size, columns: cols, pages: page_list, languages })
    }

    /// A sheet in English where it has languages (the language-less data otherwise).
    pub fn sheet(&self, name: &str) -> Result<Sheet> {
        let header = self.header(name)?;
        let lang = header.default_language();
        self.sheet_in(&header, lang)
    }

    /// A sheet in `lang` (which must be one of its languages, or `None` for a sheet without).
    pub fn sheet_in(&self, header: &Header, lang: Language) -> Result<Sheet> {
        let name = &header.name;
        let row_size = header.row_size;
        let mut rows = Vec::new();
        for &(start, _) in &header.pages {
            let path = format!("exd/{name}_{start}{}.exd", lang.suffix());
            let d = self.read(&path)?;
            if d.get(..4) != Some(b"EXDF") {
                return Err(format(&path, "not an EXD"));
            }
            let index_size = be32(&d, 8) as usize;
            let index = d.get(0x20..0x20 + index_size).ok_or_else(|| format(&path, "index out of range"))?;
            for e in index.as_chunks::<8>().0 {
                let (id, at) = (be32(e, 0), be32(e, 4) as usize);
                let base = at + 6;
                let row =
                    d.get(base..base + row_size).ok_or_else(|| format(&path, format!("row {id} out of range")))?;
                let mut values = Vec::with_capacity(header.columns.len());
                for c in &header.columns {
                    let o = c.offset as usize;
                    values.push(match c.code {
                        0 => {
                            let s = base + row_size + be32(row, o) as usize;
                            let s = s.min(d.len());
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
                        t @ 0x19..=0x20 => Value::Bool(row[o] & (1 << (t - 0x19)) != 0),
                        _ => Value::UInt(0),
                    });
                }
                rows.push((id, values));
            }
        }
        rows.sort_by_key(|r| r.0);
        Ok(Sheet { name: name.clone(), columns: header.columns.clone(), rows })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet() -> Sheet {
        Sheet {
            name: "Test".into(),
            columns: vec![Column { code: 0, offset: 0 }, Column { code: 7, offset: 4 }, Column { code: 9, offset: 8 }],
            rows: vec![(1, vec![Value::Str("a".into()), Value::UInt(5), Value::Float(1.5)])],
        }
    }

    #[test]
    fn overrides_set_and_add_rows() {
        let mut s = sheet();
        assert!(s.set(1, 1, "7"));
        assert_eq!(s.row(1).unwrap()[1], Value::UInt(7));
        assert!(!s.set(1, 1, "seven"), "not a u32");
        assert!(!s.set(1, 9, "1"), "no such column");
        assert!(s.set(0, 0, "new"));
        assert_eq!(s.rows[0].0, 0, "kept in order");
        assert_eq!(s.row(0).unwrap(), &[Value::Str("new".into()), Value::UInt(0), Value::Float(0.0)]);
        assert!(s.set(1, 2, "-2.25"));
        assert_eq!(s.row(1).unwrap()[2], Value::Float(-2.25));
    }

    #[test]
    fn column_types() {
        assert_eq!(Column { code: 0x1b, offset: 0 }.type_name(), "bit2");
        assert_eq!(Column { code: 0x1b, offset: 0 }.kind(), ColumnKind::Bool);
        assert_eq!(Column { code: 4, offset: 0 }.kind(), ColumnKind::Int);
        assert_eq!(Value::parse(ColumnKind::Bool, "true"), Some(Value::Bool(true)));
    }
}
