use anyhow::{Result, ensure};
use rusqlite::Connection;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

type Ceilings = BTreeMap<String, BTreeMap<u64, u64>>;

/// Write ahead only when a hard budget needs another window. Statistics can be
/// checkpointed independently while bytes inside a durable window are forwarded.
pub(crate) struct Journal {
    path: PathBuf,
    storage: OnceLock<Storage>,
    opening: Mutex<()>,
}

struct Storage {
    db: Mutex<Connection>,
    ceilings: Mutex<Ceilings>,
    recovered: Ceilings,
}

impl Journal {
    pub fn open(path: &Path) -> Arc<Self> {
        let journal = Arc::new(Self {
            path: path.into(),
            storage: OnceLock::new(),
            opening: Mutex::new(()),
        });
        if path.exists()
            && let Err(e) = journal.connect()
        {
            eprintln!("硬额度预留账本暂不可读，将在下一次申请时重试: {e:#}");
        }
        journal
    }

    fn connect(&self) -> Result<()> {
        let _opening = self.opening.lock().unwrap();
        if self.storage.get().is_some() {
            return Ok(());
        }
        let db = Connection::open(&self.path)?;
        db.busy_timeout(Duration::from_secs(5))?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS reservation(
                id TEXT NOT NULL, cycle INTEGER NOT NULL, ceiling INTEGER NOT NULL,
                PRIMARY KEY(id,cycle));",
        )?;
        let mut recovered = Ceilings::new();
        {
            let mut stmt = db.prepare("SELECT id,cycle,ceiling FROM reservation")?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                recovered
                    .entry(row.get(0)?)
                    .or_default()
                    .insert(row.get(1)?, row.get(2)?);
            }
        }
        let _ = self.storage.set(Storage {
            db: Mutex::new(db),
            ceilings: Mutex::new(recovered.clone()),
            recovered,
        });
        Ok(())
    }

    pub async fn ready(self: &Arc<Self>) -> Result<()> {
        if self.storage.get().is_some() {
            return Ok(());
        }
        let journal = self.clone();
        tokio::task::spawn_blocking(move || journal.connect()).await?
    }

    pub fn recovered(&self, id: &str, cycle: u64) -> u64 {
        self.storage
            .get()
            .and_then(|s| s.recovered.get(id))
            .and_then(|rows| rows.get(&cycle))
            .copied()
            .unwrap_or(0)
    }

    pub fn ceiling(&self, id: &str, cycle: u64) -> u64 {
        self.storage
            .get()
            .and_then(|s| {
                s.ceilings
                    .lock()
                    .unwrap()
                    .get(id)
                    .and_then(|rows| rows.get(&cycle))
                    .copied()
            })
            .unwrap_or(0)
    }

    // The last two fields are the cumulative debit and its authorized upper limit.
    pub async fn cover(self: &Arc<Self>, requested: &[(&str, u64, u64, u64)]) -> Result<()> {
        let mut missing = Vec::new();
        for &(id, cycle, end, limit) in requested {
            ensure!(end <= limit, "额度不足");
            if self.ceiling(id, cycle) < end {
                let headroom = (limit / 64).min(1024 * 1024);
                missing.push((
                    id.to_owned(),
                    cycle,
                    end.saturating_add(headroom).min(limit),
                ));
            }
        }
        if missing.is_empty() {
            return Ok(());
        }
        let journal = self.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let storage = journal
                .storage
                .get()
                .ok_or_else(|| anyhow::anyhow!("硬额度账本尚未就绪"))?;
            let mut db = storage.db.lock().unwrap();
            let tx = db.transaction()?;
            for (id, cycle, ceiling) in &missing {
                tx.execute(
                    "INSERT INTO reservation VALUES(?1,?2,?3)
                    ON CONFLICT(id,cycle) DO UPDATE SET ceiling=max(ceiling,excluded.ceiling)",
                    (id, cycle, ceiling),
                )?;
            }
            tx.commit()?;
            let mut ceilings = storage.ceilings.lock().unwrap();
            for (id, cycle, ceiling) in missing {
                let current = ceilings.entry(id).or_default().entry(cycle).or_default();
                *current = (*current).max(ceiling);
            }
            Ok(())
        })
        .await?
    }

    /// Only after the metering tasks have stopped and exact usage is durable.
    /// Untouched keys retain their crash bound across any number of restarts.
    pub fn settle(&self, rows: Vec<(String, u64, u64)>) -> Result<()> {
        let Some(storage) = self.storage.get() else {
            return Ok(());
        };
        let mut db = storage.db.lock().unwrap();
        let mut ceilings = storage.ceilings.lock().unwrap();
        let tx = db.transaction()?;
        let mut settled = Vec::new();
        for (id, cycle, used) in rows {
            if let Some(&previous) = ceilings.get(&id).and_then(|set| set.get(&cycle)) {
                let exact = used.max(self.recovered(&id, cycle));
                if exact < previous
                    && tx.execute(
                        "UPDATE reservation SET ceiling=?3 WHERE id=?1 AND cycle=?2 AND ceiling=?4",
                        (&id, cycle, exact, previous),
                    )? == 1
                {
                    settled.push((id, cycle, exact));
                }
            }
        }
        tx.commit()?;
        for (id, cycle, exact) in settled {
            ceilings.get_mut(&id).unwrap().insert(cycle, exact);
        }
        Ok(())
    }
}
