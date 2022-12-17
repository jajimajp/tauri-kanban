use std::{collections::BTreeMap, str::FromStr};

use futures::TryStreamExt;
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
    Row, Sqlite, SqlitePool, Transaction,
};

use crate::{Card, CardPos, Column};

type DbResult<T> = Result<T, Box<dyn std::error::Error>>;

pub(crate) async fn create_sqlite_pool(database_url: &str) -> DbResult<SqlitePool> {
    let connection_options = SqliteConnectOptions::from_str(database_url)?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal);

    let sqlite_pool = SqlitePoolOptions::new()
        .connect_with(connection_options)
        .await?;

    Ok(sqlite_pool)
}

pub(crate) async fn migrate_database(pool: &SqlitePool) -> DbResult<()> {
    sqlx::migrate!("./db").run(pool).await?;
    Ok(())
}

pub(crate) async fn get_columns(pool: &SqlitePool) -> DbResult<Vec<Column>> {
    const SQL1: &str = "SELECT * FROM columns ORDER BY id ASC";
    let mut rows = sqlx::query(SQL1).fetch(pool);

    let mut columns = BTreeMap::new();
    while let Some(row) = rows.try_next().await? {
        let id: i64 = row.try_get("id")?;
        let title: &str = row.try_get("title")?;
        columns.insert(id, Column::new(id, title));
    }

    const SQL2: &str = "SELECT cc.column_id, cards.id, cards.title, cards.description \
        FROM cards, columns_cards AS cc \
        WHERE \
            cards.id = cc.card_id \
        ORDER BY \
            cc.column_id ASC, \
            cc.card_position ASC";

    let mut rows = sqlx::query(SQL2).fetch(pool);

    while let Some(row) = rows.try_next().await? {
        let column_id: i64 = row.try_get("column_id")?;
        let id: i64 = row.try_get("id")?;
        let title: &str = row.try_get("title")?;
        let description: Option<String> = row.try_get("description")?;
        let card = Card::new(id, title, description);
        columns.get_mut(&column_id).unwrap().add_card(card);
    }

    Ok(columns.into_iter().map(|(_k, v)| v).collect())
}

pub(crate) async fn insert_card(pool: &SqlitePool, card: Card, pos: CardPos) -> DbResult<()> {
    let mut tx = pool.begin().await?;

    sqlx::query("INSERT INTO cards (id, title, description) VALUES (?, ?, ?)")
        .bind(card.id)
        .bind(card.title)
        .bind(card.description)
        .execute(&mut tx)
        .await?;

    insert_card_position(&mut tx, pos.column_id, card.id, pos.position).await?;

    tx.commit().await?;

    Ok(())
}

pub(crate) async fn move_card(
    pool: &SqlitePool,
    card: Card,
    from: CardPos,
    to: CardPos,
) -> DbResult<()> {
    let mut tx = pool.begin().await?;
    delete_card_position(&mut tx, from.column_id, card.id, from.position).await?;
    insert_card_position(&mut tx, to.column_id, card.id, to.position).await?;
    tx.commit().await?;
    Ok(())
}

pub(crate) async fn delete_card(pool: &SqlitePool, card: Card, column_id: i64) -> DbResult<()> {
  let mut tx = pool.begin().await?;

  let position = sqlx::query("SELECT card_posotion FROM columns_cards WHERE card_id = ?")
      .bind(card.id)
      .fetch_one(&mut tx)
      .await
      .and_then(|row| row.try_get::<i64, _>("card_position"))?;

  delete_card_position(&mut tx, column_id, card.id, position).await?;

  sqlx::query("DELETE FROM cards WHERE id = ?")
      .bind(card.id)
      .execute(&mut tx)
      .await?;

  tx.commit().await?;

  Ok(())
}

async fn insert_card_position(
    tx: &mut Transaction<'_, Sqlite>,
    column_id: i64,
    card_id: i64,
    position: i64,
) -> DbResult<()> {
    update_card_positions(tx, column_id, position, |pos| pos + 1).await?;

    sqlx::query("INSERT INTO columns_cards (column_id, card_id, card_position) VALUES (?, ?, ?)")
        .bind(column_id)
        .bind(card_id)
        .bind(position)
        .execute(&mut *tx)
        .await?;

    Ok(())
}


async fn delete_card_position(
    tx: &mut Transaction<'_, Sqlite>,
    column_id: i64,
    card_id: i64,
    position: i64,
) -> DbResult<()> {
    sqlx::query("DELETE FROM columns_cards WHERE card_id = ?")
        .bind(card_id)
        .execute(&mut *tx)
        .await?;

    update_card_positions(tx, column_id, position, |pos| pos - 1).await?;

    Ok(())
}

async fn update_card_positions(
    tx: &mut Transaction<'_, Sqlite>,
    column_id: i64,
    start_position: i64,
    mut new_position: impl FnMut(i64) -> i64,
) -> DbResult<()> {
    const SELECT: &str = "SELECT card_id, card_position \
        FROM columns_cards \
        WHERE column_id = ? AND card_position >= ? \
        ORDER BY card_position ASC";

    let mut rows = sqlx::query(SELECT)
        .bind(column_id)
        .bind(start_position)
        .fetch(&mut *tx);

    let mut positions = Vec::new();

    while let Some(row) = rows.try_next().await? {
        let id: i64 = row.try_get("card_id")?;
        let pos: i64 = row.try_get("card_position")?;
        positions.push((id, pos));
    }

    std::mem::drop(rows);

    const UPDATE: &str = "UPDATE columns_cards \
        SET card_position = ? \
        WHERE column_id = ? AND card_id = ?";

    for (card_id, pos) in positions {
        sqlx::query(UPDATE)
            .bind(new_position(pos))
            .bind(column_id)
            .bind(card_id)
            .execute(&mut *tx)
            .await?;
    }

    Ok(())
}


