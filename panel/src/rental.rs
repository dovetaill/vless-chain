use crate::model::*;
use anyhow::{Context, Result, ensure};
use chrono::{Datelike, FixedOffset, Months, TimeZone};
use serde_json::{Value, json};

pub const MAX_PRICE: u64 = 100_000_000_000;

pub fn add_months(anchor: u64, months: u32) -> Result<u64> {
    let zone = FixedOffset::east_opt(8 * 3600).unwrap();
    let date = zone
        .timestamp_opt(i64::try_from(anchor)?, 0)
        .single()
        .context("开通时间无效")?;
    let end = date
        .checked_add_months(Months::new(months))
        .context("租期超出有效日期范围")?;
    Ok(u64::try_from(end.timestamp())?)
}

pub fn cycle(anchor: u64, at: u64) -> (u64, u64) {
    let zone = FixedOffset::east_opt(8 * 3600).unwrap();
    let start = zone
        .timestamp_opt(anchor.min(i64::MAX as u64) as i64, 0)
        .single();
    let current = zone
        .timestamp_opt(at.min(i64::MAX as u64) as i64, 0)
        .single();
    let (Some(start), Some(current)) = (start, current) else {
        return (anchor, u64::MAX);
    };
    let mut count = ((current.year() - start.year()) * 12 + current.month() as i32
        - start.month() as i32)
        .max(0) as u32;
    if add_months(anchor, count).unwrap_or(u64::MAX) > at && count > 0 {
        count -= 1;
    }
    (
        add_months(anchor, count).unwrap_or(anchor),
        add_months(anchor, count + 1).unwrap_or(u64::MAX),
    )
}

pub fn term_months(term: &str) -> Result<u32> {
    Ok(match term {
        "monthly" => 1,
        "quarterly" => 3,
        "yearly" => 12,
        _ => anyhow::bail!("请选择月付、季付或年付"),
    })
}

pub fn price(prices: &Prices, term: &str) -> Result<u64> {
    term_months(term)?;
    match term {
        "monthly" => prices.monthly,
        "quarterly" => prices.quarterly,
        _ => prices.yearly,
    }
    .context("该代理未开放所选报价档位")
}

pub fn renew(rental: &mut Rental, term: &str, at: u64) -> Result<u64> {
    let months = term_months(term)?;
    price(&rental.prices, term)?;
    let previous = rental.expires_at;
    let (anchor, total) = if rental.started_at > 0 && rental.expires_at > at {
        (
            rental.started_at,
            rental.paid_months.checked_add(months).context("租期过长")?,
        )
    } else {
        (at, months)
    };
    ensure!(total <= 1200, "最长租期为 100 年");
    let expires_at = add_months(anchor, total)?;
    rental.started_at = anchor;
    rental.paid_months = total;
    rental.expires_at = expires_at;
    Ok(previous)
}

pub fn traffic<'a>(doc: &'a Document, line: &Line) -> Option<&'a Traffic> {
    doc.applied_lines
        .iter()
        .find(|l| l.id == line.id)
        .unwrap_or(line)
        .hops
        .first()
        .and_then(|h| doc.nodes.iter().find(|n| n.id == h.node_id))
        .and_then(|n| n.report.traffic.get(&line.id))
}

pub fn safe_traffic(
    rental: Option<&Rental>,
    traffic: Option<&Traffic>,
    at: u64,
) -> Option<Traffic> {
    let mut value = traffic?.clone();
    if let Some(rental) = rental {
        let (start, end) = cycle(rental.started_at, at);
        if value.cycle_start != start {
            value.upload = 0;
            value.download = 0;
            value.blocked = false;
            value.cycle_start = start;
            value.cycle_end = end;
        }
    }
    Some(value)
}

pub fn sync_status(doc: &Document, line: &Line) -> &'static str {
    if !doc.applied_lines.iter().any(|l| l.id == line.id) {
        return "unpublished";
    }
    let applied = doc
        .applied_lines
        .iter()
        .find(|l| l.id == line.id)
        .unwrap_or(line);
    let Some(node) = applied
        .hops
        .first()
        .and_then(|h| doc.nodes.iter().find(|n| n.id == h.node_id))
    else {
        return "unpublished";
    };
    if !node.report.error.is_empty() {
        return "error";
    }
    if applied != line {
        return "unpublished";
    }
    if now().saturating_sub(node.seen) >= 15
        || !node.report.applied
        || !node.report.running
        || node.desired.as_ref().map(|d| d.revision.as_str()) != Some(node.report.revision.as_str())
    {
        return "pending";
    }
    "synced"
}

pub fn status(doc: &Document, line: &Line, at: u64) -> &'static str {
    if !line.enabled {
        return "disabled";
    }
    if let Some(rental) = doc.rentals.get(&line.id) {
        if rental.started_at == 0 {
            return "not_started";
        }
        if rental.expires_at > 0 && rental.expires_at <= at {
            return "expired";
        }
    }
    if !doc.applied_lines.iter().any(|l| l.id == line.id) {
        return "draft";
    }
    let traffic = safe_traffic(doc.rentals.get(&line.id), traffic(doc, line), at);
    if let (Some(t), Some(limit)) = (&traffic, line.limits.monthly_gib) {
        if t.upload.saturating_add(t.download) >= (limit * 1073741824.0) as u64 {
            return "quota_exhausted";
        }
    }
    if line.hops.iter().any(|h| {
        doc.nodes
            .iter()
            .find(|n| n.id == h.node_id)
            .is_none_or(|n| at.saturating_sub(n.seen) >= 15 || !n.report.running)
    }) {
        return "offline";
    }
    "active"
}

pub fn view(doc: &Document, line: &Line, at: u64) -> Value {
    let rental = doc.rentals.get(&line.id);
    let value = rental.map(|r| {
        json!({
            "customer_id":r.customer_id,"prices":r.prices,"started_at":r.started_at,
            "expires_at":r.expires_at,"paid_months":r.paid_months,
        "permanent":r.started_at > 0 && r.expires_at == 0,
            "next_reset_at":if r.started_at > 0 { cycle(r.started_at, at).1 } else { 0 },
            "query_link_exists":!r.query_token_hash.is_empty()
        })
    });
    json!({"rental":value,"rental_status":status(doc,line,at),"sync_status":sync_status(doc,line),
        "traffic":safe_traffic(rental,traffic(doc,line),at)})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn date(s: &str) -> u64 {
        chrono::DateTime::parse_from_rfc3339(s).unwrap().timestamp() as u64
    }
    fn lease(start: u64) -> Rental {
        Rental {
            prices: Prices {
                monthly: Some(2000),
                quarterly: Some(5000),
                yearly: Some(18000),
            },
            started_at: start,
            expires_at: add_months(start, 1).unwrap(),
            paid_months: 1,
            ..Default::default()
        }
    }
    #[test]
    fn edited_configuration_is_not_reported_as_synced() {
        let mut doc = Document::default();
        let mut line: Line = serde_json::from_value(json!({
            "id":"line", "name":"proxy", "kind":"vless", "enabled":true,
            "hops":[{"node_id":"node","port":443,"uuid":"uuid"}]
        })).unwrap();
        let node: Node = serde_json::from_value(json!({
            "id":"node","name":"node","host":"192.0.2.1","token_hash":"",
            "reality":{"target":"","server_name":"","private_key":"","public_key":"","short_id":""},
            "seen":now(), "report":{"running":true,"applied":true,"revision":"r"},
            "desired":{"revision":"r","config":{}}
        })).unwrap();
        doc.nodes.push(node);
        doc.applied_lines.push(line.clone());
        assert_eq!(sync_status(&doc, &line), "synced");
        line.enabled = false;
        assert_eq!(sync_status(&doc, &line), "unpublished");
        doc.nodes[0].report.error = "apply failed".into();
        assert_eq!(sync_status(&doc, &line), "error");
        doc.nodes[0].report.error.clear();
        doc.applied_lines[0] = line.clone();
        assert_eq!(sync_status(&doc, &line), "synced");
        line.hops[0].port = 8443;
        assert_eq!(sync_status(&doc, &line), "unpublished");
    }
    #[test]
    fn calendar_months_keep_anchor_day_and_local_time() {
        let jan = date("2028-01-31T23:30:00+08:00");
        assert_eq!(
            add_months(jan, 1).unwrap(),
            date("2028-02-29T23:30:00+08:00")
        );
        assert_eq!(
            cycle(jan, date("2028-03-10T12:00:00+08:00")),
            (
                date("2028-02-29T23:30:00+08:00"),
                date("2028-03-31T23:30:00+08:00")
            )
        );
        assert_eq!(
            cycle(jan, date("2028-03-31T23:30:00+08:00")).0,
            date("2028-03-31T23:30:00+08:00")
        );
    }
    #[test]
    fn renewal_preserves_cycle_until_expiry_then_restarts() {
        let jan = date("2026-01-31T10:00:00+08:00");
        let mut r = lease(jan);
        renew(&mut r, "monthly", date("2026-02-10T00:00:00+08:00")).unwrap();
        assert_eq!(r.started_at, jan);
        assert_eq!(r.expires_at, date("2026-03-31T10:00:00+08:00"));
        let reopen = date("2026-04-05T12:00:00+08:00");
        renew(&mut r, "quarterly", reopen).unwrap();
        assert_eq!(r.started_at, reopen);
        assert_eq!(r.expires_at, date("2026-07-05T12:00:00+08:00"));
        assert_eq!(
            cycle(r.started_at, reopen).1,
            date("2026-05-05T12:00:00+08:00")
        );
    }
    #[test]
    fn reopening_does_not_display_previous_period_usage() {
        let r = lease(date("2026-10-15T00:00:00+08:00"));
        let t = Traffic {
            upload: 500,
            blocked: true,
            total_upload: 500,
            cycle_start: 1,
            ..Default::default()
        };
        let view = safe_traffic(Some(&r), Some(&t), r.started_at).unwrap();
        assert_eq!(
            (view.upload, view.total_upload, view.blocked),
            (0, 500, false)
        );
    }
}
