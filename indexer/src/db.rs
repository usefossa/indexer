use crate::{
    config::Config,
    error::{Error, Result},
    index::Record,
};
use rusqlite::{Connection, OptionalExtension, params};
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;

#[derive(Clone)]
pub struct Db {
    connection: Arc<Mutex<Connection>>,
    work: Arc<Semaphore>,
}
impl Db {
    pub async fn open(config: &Config) -> Result<Self> {
        let path = config.database.clone();
        let identity = format!(
            "{}:{}:{}:{}:{}",
            config.genesis, config.program, config.market, config.mint, config.commitment
        );
        let connection = tokio::task::spawn_blocking(move || -> Result<Connection> {
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty())
                && !parent.exists() {
                    std::fs::create_dir_all(parent).map_err(|_| Error::internal())?;
                    #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).map_err(|_| Error::internal())?; }
                }
            let conn = Connection::open(&path)?;
            #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).map_err(|_| Error::internal())?; }
            conn.busy_timeout(std::time::Duration::from_secs(5))?;
            conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;
                CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY,value TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS accounts(address TEXT PRIMARY KEY,slot INTEGER NOT NULL,version TEXT NOT NULL,owner TEXT NOT NULL,data BLOB NOT NULL,lamports TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS documents(hash TEXT NOT NULL,kind TEXT NOT NULL,body BLOB NOT NULL,PRIMARY KEY(hash,kind));
                CREATE TABLE IF NOT EXISTS challenges(hash TEXT PRIMARY KEY,wallet TEXT NOT NULL,lane TEXT NOT NULL,message TEXT NOT NULL,expires INTEGER NOT NULL);
                CREATE TABLE IF NOT EXISTS sessions(hash TEXT PRIMARY KEY,family TEXT NOT NULL,kind INTEGER NOT NULL,wallet TEXT NOT NULL,lane TEXT NOT NULL,expires INTEGER NOT NULL);
                CREATE INDEX IF NOT EXISTS sessions_family ON sessions(family);
                CREATE TABLE IF NOT EXISTS signatures(signature TEXT PRIMARY KEY,slot INTEGER NOT NULL,done INTEGER NOT NULL DEFAULT 0,attempts INTEGER NOT NULL DEFAULT 0,next_attempt INTEGER NOT NULL DEFAULT 0);
                CREATE INDEX IF NOT EXISTS signatures_pending ON signatures(done,slot);
                CREATE TABLE IF NOT EXISTS receipts(id TEXT PRIMARY KEY,owner TEXT NOT NULL,provider TEXT NOT NULL,slot INTEGER NOT NULL,body TEXT NOT NULL);
                CREATE INDEX IF NOT EXISTS receipts_owner ON receipts(owner,slot);
                CREATE INDEX IF NOT EXISTS receipts_owner_id ON receipts(owner,id);
                CREATE INDEX IF NOT EXISTS receipts_provider ON receipts(provider,slot);")?;
            let previous: Option<String> = conn.query_row("SELECT value FROM settings WHERE key='identity'", [], |r| r.get(0)).optional()?;
            if previous.is_some_and(|v| v != identity) { return Err(Error::bad("Database belongs to different protocol, network or commitment; use a separate database")); }
            conn.execute("INSERT OR IGNORE INTO settings VALUES('identity',?1)", [&identity])?;
            let retry_columns: u32=conn.query_row("SELECT count(*) FROM pragma_table_info('signatures') WHERE name='next_attempt'",[],|row|row.get(0))?;
            if retry_columns==0 {conn.execute_batch("ALTER TABLE signatures ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0; ALTER TABLE signatures ADD COLUMN next_attempt INTEGER NOT NULL DEFAULT 0;")?;}
            conn.execute_batch("CREATE INDEX IF NOT EXISTS signatures_retry ON signatures(done,next_attempt,slot)")?;
            Ok(conn)
        }).await.map_err(|_| Error::internal())??;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            work: Arc::new(Semaphore::new(32)),
        })
    }
    pub async fn call<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let permit = self
            .work
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| Error::internal())?;
        let conn = self.connection.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut connection = conn.lock().map_err(|_| Error::internal())?;
            f(&mut connection)
        })
        .await
        .map_err(|_| Error::internal())?
    }
    pub async fn accounts(&self, records: Vec<(String, Record)>, floor: Option<u64>) -> Result<()> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            {
                let mut statement =
                    tx.prepare_cached("INSERT OR REPLACE INTO accounts VALUES(?1,?2,?3,?4,?5,?6)")?;
                for (address, record) in records {
                    statement.execute(params![
                        address,
                        record.slot,
                        record.version.to_string(),
                        record.owner,
                        record.data.as_ref(),
                        record.lamports.to_string()
                    ])?;
                }
            }
            if let Some(slot) = floor {
                tx.execute(
                    "DELETE FROM accounts WHERE slot<=?1 AND length(data)=0",
                    [slot],
                )?;
                tx.execute(
                    "INSERT OR REPLACE INTO settings VALUES('floor',?1)",
                    [slot.to_string()],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }
    pub async fn get(&self, key: &str) -> Result<Option<String>> {
        let key = key.to_owned();
        self.call(move |conn| {
            Ok(conn
                .query_row("SELECT value FROM settings WHERE key=?1", [key], |r| {
                    r.get(0)
                })
                .optional()?)
        })
        .await
    }
}
