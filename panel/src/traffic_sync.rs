use crate::model::Traffic;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
use uuid::Uuid;

pub type Rows = BTreeMap<String, Traffic>;
pub const PAGE_BYTES: usize = 256 * 1024;
const STAGING_BYTES: usize = 64 * 1024 * 1024;
#[derive(Deserialize, Serialize)]
pub struct Heartbeat<T> {
    pub session: Uuid,
    pub report: T,
}

#[derive(Deserialize, Serialize)]
pub struct Page {
    pub session: Uuid,
    pub sequence: u64,
    pub part: u32,
    pub last: bool,
    pub rows: Rows,
}

pub fn pages(session: Uuid, sequence: u64, rows: Rows) -> impl Iterator<Item = Result<Page>> {
    let mut rows = rows.into_iter().peekable();
    let mut part = 0;
    let mut finished = false;
    std::iter::from_fn(move || {
        if finished {
            return None;
        }
        let mut page = Page {
            session,
            sequence,
            part,
            last: false,
            rows: Rows::new(),
        };
        let mut bytes = 192; // Includes the UUID, both integer fields and the JSON envelope.
        while let Some((key, row)) = rows.peek() {
            let size = match serde_json::to_vec(&(key, row)) {
                Ok(encoded) => encoded.len(),
                Err(e) => {
                    finished = true;
                    return Some(Err(e.into()));
                }
            };
            if size + bytes > PAGE_BYTES {
                if page.rows.is_empty() {
                    finished = true;
                    return Some(Err(anyhow::anyhow!("单条线路的流量历史超过分页容量")));
                }
                break;
            }
            bytes += size;
            let (key, row) = rows.next().unwrap();
            page.rows.insert(key, row);
        }
        finished = rows.peek().is_none();
        page.last = finished;
        part += 1;
        Some(Ok(page))
    })
}

pub struct Incoming {
    pub session: Uuid,
    sequence: u64,
    next: u32,
    complete: bool,
    rows: Rows,
    bytes: usize,
    touched: Instant,
}

impl Incoming {
    pub fn new(session: Uuid) -> Self {
        Self {
            session,
            sequence: 0,
            next: 0,
            complete: false,
            rows: Rows::new(),
            bytes: 0,
            touched: Instant::now(),
        }
    }
    pub fn expire(&mut self) {
        if !self.rows.is_empty() && self.touched.elapsed() > Duration::from_secs(120) {
            self.discard();
        }
    }
    pub fn discard(&mut self) {
        self.rows.clear();
        self.bytes = 0;
        self.next = 0;
        self.complete = false;
    }
    pub fn receive(&mut self, mut page: Page) -> Result<Option<Rows>> {
        self.expire();
        ensure!(
            page.session == self.session && page.sequence > 0 && page.sequence >= self.sequence,
            "流量快照已过期"
        );
        if page.sequence > self.sequence {
            ensure!(page.part == 0, "请从流量快照首页重传");
            self.discard();
            self.sequence = page.sequence;
        }
        if page.part < self.next {
            return Ok(None);
        } // The acknowledgement was lost.
        ensure!(!self.complete && page.part == self.next, "流量分页顺序错误");
        ensure!(
            !page.rows.is_empty() || page.last && page.part == 0,
            "流量分页为空"
        );
        if let (Some((last, _)), Some((first, _))) =
            (self.rows.last_key_value(), page.rows.first_key_value())
        {
            ensure!(last < first, "流量分页包含重复线路");
        }
        let size = serde_json::to_vec(&page)?.len();
        ensure!(
            size <= PAGE_BYTES && self.bytes + size <= STAGING_BYTES,
            "节点流量快照超过接收容量"
        );
        self.bytes += size;
        self.rows.append(&mut page.rows);
        self.next += 1;
        self.touched = Instant::now();
        self.complete = page.last;
        Ok(page.last.then(|| std::mem::take(&mut self.rows)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_history_is_published_once_after_all_byte_bounded_pages() {
        let session = Uuid::new_v4();
        let dates = (0..35)
            .map(|day| {
                (
                    format!("2026-{:02}-{:02}", 9 + day / 28, 1 + day % 28),
                    [12345678, 98765432],
                )
            })
            .collect();
        let row = Traffic {
            epoch: session.to_string(),
            days: dates,
            ..Default::default()
        };
        let rows: Rows = (0..1000)
            .map(|i| (format!("line-{i:04}"), row.clone()))
            .collect();
        assert!(serde_json::to_vec(&rows).unwrap().len() > 1024 * 1024);
        let mut incoming = Incoming::new(session);
        let encoded: Vec<_> = pages(session, 1, rows.clone())
            .map(|p| serde_json::to_vec(&p.unwrap()).unwrap())
            .collect();
        assert!(encoded.len() > 1 && encoded.iter().all(|p| p.len() <= PAGE_BYTES));
        let decode = |i: usize| serde_json::from_slice::<Page>(&encoded[i]).unwrap();
        assert!(incoming.receive(decode(1)).is_err());
        for i in 0..encoded.len() {
            let result = incoming.receive(decode(i)).unwrap();
            if i + 1 == encoded.len() {
                assert_eq!(result.unwrap(), rows);
            } else {
                assert!(result.is_none());
            }
            assert!(incoming.receive(decode(i)).unwrap().is_none());
        }
        assert!(
            incoming
                .receive(
                    pages(Uuid::new_v4(), 2, Rows::new())
                        .next()
                        .unwrap()
                        .unwrap()
                )
                .is_err()
        );
        assert_eq!(
            incoming
                .receive(pages(session, 2, Rows::new()).next().unwrap().unwrap())
                .unwrap(),
            Some(Rows::new())
        );
        assert!(incoming.receive(decode(0)).is_err());
    }
}
