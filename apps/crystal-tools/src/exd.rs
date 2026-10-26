//! `crystal-tools exd`: the client's EXD sheets, as the server reads them (with the world
//! database's overrides, unless `--raw`).

use anyhow::{Context, bail};
use clap::Subcommand;
use crystal_data::{Language, Sheet, Value, WorldData};

#[derive(Subcommand)]
pub enum ExdCommand {
    /// The sheets the client has, optionally only those whose name contains `filter`.
    List { filter: Option<String> },
    /// A sheet's columns, pages and languages.
    Header { sheet: String },
    /// Rows of a sheet, from `--from` on.
    Rows {
        sheet: String,
        #[arg(long, default_value_t = 0)]
        from: u32,
        #[arg(long, short = 'n', default_value_t = 20)]
        count: usize,
        /// en, ja, de, fr (the sheet's default when not given).
        #[arg(long)]
        lang: Option<String>,
        /// The client's rows, without the server's overrides.
        #[arg(long)]
        raw: bool,
    },
    /// One row, a column per line.
    Row {
        sheet: String,
        id: u32,
        #[arg(long)]
        lang: Option<String>,
        #[arg(long)]
        raw: bool,
    },
    /// Rows whose column `col` is `value` (or contains it, for text): `find BNpcName 0=antelope`.
    Find {
        sheet: String,
        /// `<column>=<value>`
        query: String,
        #[arg(long, short = 'n', default_value_t = 50)]
        count: usize,
        #[arg(long)]
        raw: bool,
    },
}

fn sheet(data: &WorldData, name: &str, lang: Option<&str>, raw: bool) -> anyhow::Result<std::sync::Arc<Sheet>> {
    let pack = data.pack();
    let name = canonical(data, name)?;
    match (lang, raw) {
        (None, false) => Ok(data.sheet(&name)?),
        (lang, _) => {
            let header = pack.header(&name)?;
            let lang = match lang {
                Some(l) => Language::from_name(l).with_context(|| format!("no language {l} (en, ja, de, fr)"))?,
                None => header.default_language(),
            };
            if !header.languages.contains(&lang) {
                bail!("{name} has no {lang:?} data; it has {:?}", header.languages);
            }
            Ok(std::sync::Arc::new(pack.sheet_in(&header, lang)?))
        }
    }
}

/// The sheet's name as the client spells it (names are matched ignoring case).
fn canonical(data: &WorldData, name: &str) -> anyhow::Result<String> {
    let names = data.pack().sheet_names()?;
    names.into_iter().find(|n| n.eq_ignore_ascii_case(name)).with_context(|| format!("no sheet {name}"))
}

fn show(v: &Value) -> String {
    match v {
        Value::Str(s) => format!("{s:?}"),
        other => other.to_string(),
    }
}

pub fn run(data: &WorldData, command: ExdCommand) -> anyhow::Result<()> {
    match command {
        ExdCommand::List { filter } => {
            let names = data.pack().sheet_names()?;
            let f = filter.as_deref().map(str::to_ascii_lowercase);
            let shown: Vec<_> =
                names.iter().filter(|n| f.as_ref().is_none_or(|f| n.to_ascii_lowercase().contains(f))).collect();
            for n in &shown {
                println!("{n}");
            }
            eprintln!("{} of {} sheets", shown.len(), names.len());
        }
        ExdCommand::Header { sheet } => {
            let h = data.pack().header(&canonical(data, &sheet)?)?;
            println!("{}: {} rows, row size {:#x}, languages {:?}", h.name, h.rows(), h.row_size, h.languages);
            for (start, rows) in &h.pages {
                println!("  page {start}: {rows} rows");
            }
            for (i, c) in h.columns.iter().enumerate() {
                println!("  {i:3}  {:6} @{:#06x}", c.type_name(), c.offset);
            }
        }
        ExdCommand::Rows { sheet: name, from, count, lang, raw } => {
            let s = sheet(data, &name, lang.as_deref(), raw)?;
            for (id, values) in s.rows.iter().filter(|r| r.0 >= from).take(count) {
                let cols: Vec<String> = values.iter().map(show).collect();
                println!("{id:7}  {}", cols.join(" | "));
            }
        }
        ExdCommand::Row { sheet: name, id, lang, raw } => {
            let s = sheet(data, &name, lang.as_deref(), raw)?;
            let values = s.row(id).with_context(|| format!("{} has no row {id}", s.name))?;
            for (i, (c, v)) in s.columns.iter().zip(values).enumerate() {
                println!("{i:3}  {:6} {}", c.type_name(), show(v));
            }
        }
        ExdCommand::Find { sheet: name, query, count, raw } => {
            let (col, want) = query.split_once('=').context("the query is <column>=<value>")?;
            let col: usize = col.trim().parse().context("the column is a number")?;
            let s = sheet(data, &name, None, raw)?;
            if col >= s.columns.len() {
                bail!("{} has {} columns", s.name, s.columns.len());
            }
            let want_lc = want.to_ascii_lowercase();
            let hits = s.rows.iter().filter(|(_, v)| match &v[col] {
                Value::Str(t) => t.to_ascii_lowercase().contains(&want_lc),
                other => other.to_string() == want.trim(),
            });
            let mut n = 0;
            for (id, values) in hits.take(count) {
                let cols: Vec<String> = values.iter().map(show).collect();
                println!("{id:7}  {}", cols.join(" | "));
                n += 1;
            }
            eprintln!("{n} rows");
        }
    }
    Ok(())
}
