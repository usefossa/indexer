use crate::{App, auth, db::Db, error::Result};
use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, header},
    response::{IntoResponse, Response},
};
use futures_util::stream;
use rusqlite::{OptionalExtension, params};
use serde_json::json;

struct History {
    db: Db,
    owner: String,
    ceiling: String,
    cursor: String,
    started: bool,
    seen: bool,
    done: bool,
}

pub async fn native(State(app): State<App>, headers: HeaderMap) -> Result<Response> {
    let owner = auth::session(&app, &headers).await?.wallet;
    let ceiling = app
        .db
        .call({
            let owner = owner.clone();
            move |conn| {
                Ok(conn
                    .query_row(
                        "SELECT id FROM receipts WHERE owner=?1 ORDER BY id DESC LIMIT 1",
                        [owner],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?
                    .unwrap_or_default())
            }
        })
        .await?;
    let history = History {
        db: app.db.clone(),
        owner,
        ceiling,
        cursor: String::new(),
        started: false,
        seen: false,
        done: false,
    };
    Ok((
        [(header::CONTENT_TYPE, "application/json")],
        Body::from_stream(stream::try_unfold(history, page)),
    )
        .into_response())
}

async fn page(mut history: History) -> Result<Option<(Bytes, History)>> {
    if history.done {
        return Ok(None);
    }
    let owner = history.owner.clone();
    let cursor = history.cursor.clone();
    let ceiling = history.ceiling.clone();
    let rows = history.db.call(move |conn| {
        let mut query = conn.prepare_cached(
            "SELECT id,json_extract(body,'$.booking'),json_extract(body,'$.generation'),json_extract(body,'$.charge')
             FROM receipts WHERE owner=?1 AND id>?2 AND id<=?3 ORDER BY id LIMIT 256",
        )?;
        Ok(query.query_map(params![owner, cursor, ceiling], |row| Ok((
            row.get::<_, String>(0)?, row.get::<_, String>(1)?,
            row.get::<_, String>(2)?, row.get::<_, String>(3)?,
        )))?.collect::<std::result::Result<Vec<_>, _>>()?)
    }).await?;
    let mut chunk = if history.started {
        String::new()
    } else {
        "{\"receipts\":[".into()
    };
    history.started = true;
    history.done = rows.len() < 256;
    for (id, booking, generation, charge) in rows {
        if history.seen {
            chunk.push(',');
        }
        history.seen = true;
        chunk.push_str(
            &json!({"booking":booking,"generation":generation,"charge":charge}).to_string(),
        );
        history.cursor = id;
    }
    if history.done {
        chunk.push_str("],\"coverage\":{\"complete\":false,\"commitment\":\"finalized\"}}");
    }
    Ok(Some((chunk.into(), history)))
}
