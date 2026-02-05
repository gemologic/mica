use crate::schema::SCHEMA;
use rusqlite::{params, Connection};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("failed to open database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("failed to read input: {0}")]
    Read(std::io::Error),
    #[error("failed to parse json: {0}")]
    Json(serde_json::Error),
}

#[derive(Debug, Deserialize)]
pub struct NixPackage {
    pub attr_path: String,
    pub name: String,
    pub version: Option<String>,
    pub description: Option<String>,
    pub homepage: Option<String>,
    pub license: Option<serde_json::Value>,
    pub platforms: Option<serde_json::Value>,
    pub main_program: Option<String>,
    pub position: Option<String>,
    pub broken: Option<bool>,
    pub insecure: Option<bool>,
}

pub fn init_db(path: &Path) -> Result<Connection, IndexError> {
    let conn = Connection::open(path)?;
    conn.execute_batch(SCHEMA)?;
    ensure_packages_columns(&conn)?;
    Ok(conn)
}

pub fn open_db(path: &Path) -> Result<Connection, IndexError> {
    let conn = Connection::open(path)?;
    ensure_packages_columns(&conn)?;
    Ok(conn)
}

fn ensure_packages_columns(conn: &Connection) -> Result<(), IndexError> {
    let mut stmt = conn.prepare("PRAGMA table_info(packages)")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    let mut columns = HashSet::new();
    for row in rows {
        columns.insert(row?);
    }
    if !columns.contains("position") {
        conn.execute("ALTER TABLE packages ADD COLUMN position TEXT", [])?;
    }
    Ok(())
}

pub fn ingest_packages(conn: &mut Connection, packages: &[NixPackage]) -> Result<(), IndexError> {
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO packages (attr_path, name, version, description, homepage, license, platforms, main_program, position, broken, insecure) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        )?;
        for pkg in packages {
            let license_json = pkg.license.as_ref().map(|v| v.to_string());
            let platforms_json = pkg.platforms.as_ref().map(|v| v.to_string());
            stmt.execute(params![
                pkg.attr_path,
                pkg.name,
                pkg.version,
                pkg.description,
                pkg.homepage,
                license_json,
                platforms_json,
                pkg.main_program,
                pkg.position,
                pkg.broken.unwrap_or(false) as i32,
                pkg.insecure.unwrap_or(false) as i32,
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

pub fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<(), IndexError> {
    conn.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
        params![key, value],
    )?;
    Ok(())
}

pub fn get_meta(conn: &Connection) -> Result<Vec<(String, String)>, IndexError> {
    let mut stmt = conn.prepare("SELECT key, value FROM meta ORDER BY key")?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

pub fn load_packages_from_json(path: &Path) -> Result<Vec<NixPackage>, IndexError> {
    let content = std::fs::read_to_string(path).map_err(IndexError::Read)?;
    let value: serde_json::Value = serde_json::from_str(&content).map_err(IndexError::Json)?;
    let mut packages = Vec::new();
    let obj = match value {
        serde_json::Value::Object(obj) => obj,
        _ => return Ok(packages),
    };

    for (attr_path, entry) in obj {
        let entry = match entry {
            serde_json::Value::Object(map) => map,
            _ => continue,
        };

        let name = entry
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or(&attr_path)
            .to_string();
        let version = entry
            .get("version")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let meta = entry.get("meta").and_then(|v| v.as_object());

        let description = entry
            .get("description")
            .and_then(|v| v.as_str())
            .or_else(|| meta.and_then(|m| m.get("description").and_then(|v| v.as_str())))
            .or_else(|| meta.and_then(|m| m.get("longDescription").and_then(|v| v.as_str())))
            .map(|s| s.to_string());
        let homepage = entry
            .get("homepage")
            .and_then(|v| v.as_str())
            .or_else(|| meta.and_then(|m| m.get("homepage").and_then(|v| v.as_str())))
            .map(|s| s.to_string());
        let license = entry
            .get("license")
            .cloned()
            .or_else(|| meta.and_then(|m| m.get("license").cloned()));
        let platforms = entry
            .get("platforms")
            .cloned()
            .or_else(|| meta.and_then(|m| m.get("platforms").cloned()));
        let main_program = entry
            .get("mainProgram")
            .and_then(|v| v.as_str())
            .or_else(|| meta.and_then(|m| m.get("mainProgram").and_then(|v| v.as_str())))
            .map(|s| s.to_string());
        let position = entry
            .get("position")
            .and_then(|v| v.as_str())
            .or_else(|| meta.and_then(|m| m.get("position").and_then(|v| v.as_str())))
            .map(|s| s.to_string());
        let broken = entry
            .get("broken")
            .and_then(|v| v.as_bool())
            .or_else(|| meta.and_then(|m| m.get("broken").and_then(|v| v.as_bool())));
        let insecure = entry
            .get("insecure")
            .and_then(|v| v.as_bool())
            .or_else(|| meta.and_then(|m| m.get("insecure").and_then(|v| v.as_bool())));

        packages.push(NixPackage {
            attr_path,
            name,
            version,
            description,
            homepage,
            license,
            platforms,
            main_program,
            position,
            broken,
            insecure,
        });
    }

    Ok(packages)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageInfo {
    pub attr_path: String,
    pub name: String,
    pub version: Option<String>,
    pub description: Option<String>,
    pub homepage: Option<String>,
    pub license: Option<String>,
    pub platforms: Option<String>,
    pub main_program: Option<String>,
    pub position: Option<String>,
    pub broken: bool,
    pub insecure: bool,
}

pub fn search_packages(
    conn: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<PackageInfo>, IndexError> {
    let fts_query = format!("{}*", query.replace(' ', " OR "));
    let mut stmt = conn.prepare(
        "SELECT p.attr_path, p.name, p.version, p.description, p.homepage, p.license, p.platforms, p.main_program, p.position, p.broken, p.insecure \
         FROM packages p \
         JOIN packages_fts fts ON p.id = fts.rowid \
         WHERE packages_fts MATCH ?1 \
         ORDER BY rank \
         LIMIT ?2",
    )?;
    let rows = stmt.query_map([fts_query, limit.to_string()], |row| {
        Ok(PackageInfo {
            attr_path: row.get(0)?,
            name: row.get(1)?,
            version: row.get(2)?,
            description: row.get(3)?,
            homepage: row.get(4)?,
            license: row.get(5)?,
            platforms: row.get(6)?,
            main_program: row.get(7)?,
            position: row.get(8)?,
            broken: row.get::<_, i32>(9)? != 0,
            insecure: row.get::<_, i32>(10)? != 0,
        })
    })?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

pub fn list_packages(conn: &Connection, limit: usize) -> Result<Vec<PackageInfo>, IndexError> {
    let mut stmt = conn.prepare(
        "SELECT attr_path, name, version, description, homepage, license, platforms, main_program, position, broken, insecure \
         FROM packages ORDER BY name LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit.to_string()], |row| {
        Ok(PackageInfo {
            attr_path: row.get(0)?,
            name: row.get(1)?,
            version: row.get(2)?,
            description: row.get(3)?,
            homepage: row.get(4)?,
            license: row.get(5)?,
            platforms: row.get(6)?,
            main_program: row.get(7)?,
            position: row.get(8)?,
            broken: row.get::<_, i32>(9)? != 0,
            insecure: row.get::<_, i32>(10)? != 0,
        })
    })?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}
