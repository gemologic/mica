use crate::generate::{IndexError, NixPackage};
use rusqlite::{params, Connection};
use std::path::Path;

pub const VERSIONS_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS package_versions (
    id INTEGER PRIMARY KEY,
    attr_path TEXT NOT NULL,
    version TEXT NOT NULL,
    source TEXT NOT NULL,
    commit_rev TEXT NOT NULL,
    commit_date TEXT NOT NULL,
    branch TEXT NOT NULL,
    UNIQUE(attr_path, version, source, commit_rev)
);

CREATE TABLE IF NOT EXISTS indexed_commits (
    source TEXT NOT NULL,
    commit_rev TEXT NOT NULL,
    branch TEXT NOT NULL,
    commit_date TEXT NOT NULL,
    indexed_at TEXT NOT NULL,
    package_count INTEGER,
    url TEXT NOT NULL,
    PRIMARY KEY (source, commit_rev)
);
"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageVersion {
    pub source: String,
    pub version: String,
    pub commit: String,
    pub commit_date: String,
    pub branch: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionSource {
    pub source: String,
    pub url: String,
    pub branch: String,
    pub commit: String,
    pub commit_date: String,
    pub indexed_at: String,
}

pub fn init_versions_db(path: &Path) -> Result<Connection, IndexError> {
    let conn = Connection::open(path)?;
    conn.execute_batch(VERSIONS_SCHEMA)?;
    Ok(conn)
}

pub fn open_versions_db(path: &Path) -> Result<Connection, IndexError> {
    let conn = Connection::open(path)?;
    conn.execute_batch(VERSIONS_SCHEMA)?;
    Ok(conn)
}

pub fn record_versions(
    conn: &mut Connection,
    source: &VersionSource,
    packages: &[NixPackage],
) -> Result<(), IndexError> {
    let tx = conn.transaction()?;
    tx.execute(
        "INSERT OR REPLACE INTO indexed_commits (source, commit_rev, branch, commit_date, indexed_at, package_count, url) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            source.source,
            source.commit,
            source.branch,
            source.commit_date,
            source.indexed_at,
            packages.len() as i64,
            source.url
        ],
    )?;

    {
        let mut stmt = tx.prepare(
            "INSERT OR IGNORE INTO package_versions (attr_path, version, source, commit_rev, commit_date, branch) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for pkg in packages {
            let Some(version) = pkg.version.as_deref().filter(|v| !v.trim().is_empty()) else {
                continue;
            };
            stmt.execute(params![
                pkg.attr_path,
                version,
                source.source,
                source.commit,
                source.commit_date,
                source.branch
            ])?;
        }
    }

    tx.commit()?;
    Ok(())
}

pub fn list_versions(
    conn: &Connection,
    attr_path: &str,
    limit: usize,
) -> Result<Vec<PackageVersion>, IndexError> {
    let mut stmt = conn.prepare(
        "SELECT v.source, v.version, v.commit_rev, v.commit_date, v.branch, c.url \
         FROM package_versions v \
         JOIN indexed_commits c ON v.source = c.source AND v.commit_rev = c.commit_rev \
         WHERE v.attr_path = ?1 \
         ORDER BY v.commit_date DESC, v.version DESC \
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![attr_path, limit as i64], |row| {
        Ok(PackageVersion {
            source: row.get(0)?,
            version: row.get(1)?,
            commit: row.get(2)?,
            commit_date: row.get(3)?,
            branch: row.get(4)?,
            url: row.get(5)?,
        })
    })?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

pub fn version_for_commit(
    conn: &Connection,
    attr_path: &str,
    source: &str,
    commit: &str,
) -> Result<Option<PackageVersion>, IndexError> {
    let mut stmt = conn.prepare(
        "SELECT v.source, v.version, v.commit_rev, v.commit_date, v.branch, c.url \
         FROM package_versions v \
         JOIN indexed_commits c ON v.source = c.source AND v.commit_rev = c.commit_rev \
         WHERE v.attr_path = ?1 AND v.source = ?2 AND v.commit_rev = ?3 \
         ORDER BY v.commit_date DESC, v.version DESC \
         LIMIT 1",
    )?;
    let mut rows = stmt.query(params![attr_path, source, commit])?;
    if let Some(row) = rows.next()? {
        Ok(Some(PackageVersion {
            source: row.get(0)?,
            version: row.get(1)?,
            commit: row.get(2)?,
            commit_date: row.get(3)?,
            branch: row.get(4)?,
            url: row.get(5)?,
        }))
    } else {
        Ok(None)
    }
}

pub fn latest_version_for_source(
    conn: &Connection,
    attr_path: &str,
    source: &str,
) -> Result<Option<PackageVersion>, IndexError> {
    let mut stmt = conn.prepare(
        "SELECT v.source, v.version, v.commit_rev, v.commit_date, v.branch, c.url \
         FROM package_versions v \
         JOIN indexed_commits c ON v.source = c.source AND v.commit_rev = c.commit_rev \
         WHERE v.attr_path = ?1 AND v.source = ?2 \
         ORDER BY v.commit_date DESC, v.version DESC \
         LIMIT 1",
    )?;
    let mut rows = stmt.query(params![attr_path, source])?;
    if let Some(row) = rows.next()? {
        Ok(Some(PackageVersion {
            source: row.get(0)?,
            version: row.get(1)?,
            commit: row.get(2)?,
            commit_date: row.get(3)?,
            branch: row.get(4)?,
            url: row.get(5)?,
        }))
    } else {
        Ok(None)
    }
}
