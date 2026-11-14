//! Launch history (count + last used) in redb at `<state dir>/apps.redb`.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};

const LAUNCHES: TableDefinition<&str, (u32, u64)> = TableDefinition::new("launches");

/// `$ASHD_STATE_DIR` or `$XDG_STATE_HOME/ashd` or `~/.local/state/ashd` (same rule as
/// `ashd_tokens::state::state_dir`).
pub fn state_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("ASHD_STATE_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(dir);
    }
    dirs::state_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("ashd")
}

pub struct History {
    db: Option<Database>,
}

impl History {
    /// Never fails: without a database (read-only state dir, another process holding the
    /// lock) launches are only counted in memory.
    pub fn open(path: &Path) -> Self {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match Database::create(path) {
            Ok(db) => Self { db: Some(db) },
            Err(e) => {
                tracing::warn!("launch history disabled ({}): {e}", path.display());
                Self { db: None }
            }
        }
    }

    pub fn load(&self) -> HashMap<String, (u32, u64)> {
        let Some(db) = &self.db else { return HashMap::new() };
        let run = || -> Result<HashMap<String, (u32, u64)>, redb::Error> {
            let tx = db.begin_read()?;
            let table = match tx.open_table(LAUNCHES) {
                Ok(t) => t,
                Err(redb::TableError::TableDoesNotExist(_)) => return Ok(HashMap::new()),
                Err(e) => return Err(e.into()),
            };
            let mut out = HashMap::new();
            for row in table.iter()? {
                let (k, v) = row?;
                out.insert(k.value().to_string(), v.value());
            }
            Ok(out)
        };
        run().unwrap_or_else(|e| {
            tracing::warn!("reading launch history: {e}");
            HashMap::new()
        })
    }

    pub fn store(&self, id: &str, value: (u32, u64)) {
        let Some(db) = &self.db else { return };
        let run = || -> Result<(), redb::Error> {
            let tx = db.begin_write()?;
            {
                let mut table = tx.open_table(LAUNCHES)?;
                table.insert(id, value)?;
            }
            tx.commit()?;
            Ok(())
        };
        if let Err(e) = run() {
            tracing::warn!("writing launch history: {e}");
        }
    }
}
