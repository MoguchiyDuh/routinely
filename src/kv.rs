use std::time::Duration;

use redis::AsyncCommands;
use redis::aio::ConnectionManager;

pub use redis::RedisError as KvError;

#[derive(Clone)]
pub struct Kv {
    conn: ConnectionManager,
    prefix: String,
}

impl Kv {
    pub async fn connect(url: &str, prefix: &str) -> Result<Self, KvError> {
        let client = redis::Client::open(url)?;
        let conn = ConnectionManager::new(client).await?;
        return Ok(Self {
            conn,
            prefix: prefix.to_string(),
        });
    }

    fn key(&self, parts: &[&str]) -> String {
        let mut key = self.prefix.clone();
        for (index, part) in parts.iter().enumerate() {
            if index > 0 {
                key.push(':');
            }
            key.push_str(part);
        }
        return key;
    }

    pub async fn ping(&self) -> Result<(), KvError> {
        let mut conn = self.conn.clone();
        let _: String = redis::cmd("PING").query_async(&mut conn).await?;
        return Ok(());
    }

    pub async fn session_put(
        &self,
        token_hash: &str,
        user_id: i64,
        ttl: Duration,
    ) -> Result<(), KvError> {
        let secs = ttl.as_secs().max(1);
        let session_key = self.key(&["session", token_hash]);
        let index_key = self.key(&["user_sessions", &user_id.to_string()]);
        let mut conn = self.conn.clone();
        let _: () = redis::pipe()
            .atomic()
            .set_ex(&session_key, user_id, secs)
            .ignore()
            .sadd(&index_key, token_hash)
            .ignore()
            .expire(&index_key, i64::try_from(secs).unwrap_or(i64::MAX))
            .ignore()
            .query_async(&mut conn)
            .await?;
        return Ok(());
    }

    pub async fn session_user(&self, token_hash: &str) -> Result<Option<i64>, KvError> {
        let mut conn = self.conn.clone();
        let user_id: Option<i64> = conn.get(self.key(&["session", token_hash])).await?;
        return Ok(user_id);
    }

    pub async fn session_delete(&self, token_hash: &str) -> Result<(), KvError> {
        let session_key = self.key(&["session", token_hash]);
        let mut conn = self.conn.clone();
        let user_id: Option<i64> = conn.get(&session_key).await?;
        let mut pipe = redis::pipe();
        pipe.atomic().del(&session_key).ignore();
        if let Some(user_id) = user_id {
            pipe.srem(self.key(&["user_sessions", &user_id.to_string()]), token_hash)
                .ignore();
        }
        let _: () = pipe.query_async(&mut conn).await?;
        return Ok(());
    }

    pub async fn session_delete_others(
        &self,
        user_id: i64,
        keep_hash: &str,
    ) -> Result<(), KvError> {
        let index_key = self.key(&["user_sessions", &user_id.to_string()]);
        let mut conn = self.conn.clone();
        let hashes: Vec<String> = conn.smembers(&index_key).await?;
        let mut pipe = redis::pipe();
        pipe.atomic();
        for hash in hashes.iter().filter(|hash| hash.as_str() != keep_hash) {
            pipe.del(self.key(&["session", hash])).ignore();
            pipe.srem(&index_key, hash).ignore();
        }
        let _: () = pipe.query_async(&mut conn).await?;
        return Ok(());
    }

    pub async fn session_count(&self, user_id: i64) -> Result<usize, KvError> {
        let index_key = self.key(&["user_sessions", &user_id.to_string()]);
        let mut conn = self.conn.clone();
        let hashes: Vec<String> = conn.smembers(&index_key).await?;
        let mut live = 0;
        for hash in hashes {
            let exists: bool = conn.exists(self.key(&["session", &hash])).await?;
            if exists {
                live += 1;
            }
        }
        return Ok(live);
    }
}
