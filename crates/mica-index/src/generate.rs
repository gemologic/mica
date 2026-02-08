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
    conn.execute_batch(SCHEMA)?;
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
    tx.execute("DELETE FROM package_binaries", [])?;
    tx.execute("DELETE FROM packages", [])?;
    tx.execute(
        "INSERT INTO packages_fts(packages_fts) VALUES('delete-all')",
        [],
    )?;
    {
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO packages (attr_path, name, version, description, homepage, license, platforms, main_program, position, broken, insecure) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        )?;
        let mut bin_stmt =
            tx.prepare("INSERT INTO package_binaries (package_id, binary_name) VALUES (?1, ?2)")?;
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
            let pkg_id = tx.last_insert_rowid();
            if let Some(main_program) = pkg
                .main_program
                .as_deref()
                .filter(|value| !value.trim().is_empty())
            {
                bin_stmt.execute(params![pkg_id, main_program])?;
            }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    Name,
    Description,
    Binary,
    All,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedSearch {
    query: String,
    mode: SearchMode,
    exact: bool,
}

pub fn search_packages(
    conn: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<PackageInfo>, IndexError> {
    search_packages_with_mode(conn, query, limit, SearchMode::All)
}

pub fn search_packages_with_mode(
    conn: &Connection,
    query: &str,
    limit: usize,
    mode: SearchMode,
) -> Result<Vec<PackageInfo>, IndexError> {
    let parsed = parse_search_shortcuts(query, mode);
    if parsed.query.is_empty() {
        return Ok(Vec::new());
    }

    match (parsed.mode, parsed.exact) {
        (SearchMode::Name, false) => search_packages_fts(conn, &parsed.query, limit, Some("name")),
        (SearchMode::Description, false) => {
            search_packages_fts(conn, &parsed.query, limit, Some("description"))
        }
        (SearchMode::Binary, false) => search_packages_by_binary(conn, &parsed.query, limit),
        (SearchMode::Name, true) => search_packages_by_name_exact(conn, &parsed.query, limit),
        (SearchMode::Description, true) => {
            search_packages_by_description_exact(conn, &parsed.query, limit)
        }
        (SearchMode::Binary, true) => search_packages_by_binary_exact(conn, &parsed.query, limit),
        (SearchMode::All, false) => {
            let mut results = search_packages_fts(conn, &parsed.query, limit, None)?;
            if results.len() < limit {
                append_unique_by_attr(
                    &mut results,
                    search_packages_by_binary(conn, &parsed.query, limit)?,
                    limit,
                );
            }
            Ok(results)
        }
        (SearchMode::All, true) => {
            let mut results = search_packages_by_name_exact(conn, &parsed.query, limit)?;
            if results.len() < limit {
                append_unique_by_attr(
                    &mut results,
                    search_packages_by_description_exact(conn, &parsed.query, limit)?,
                    limit,
                );
            }
            if results.len() < limit {
                append_unique_by_attr(
                    &mut results,
                    search_packages_by_binary_exact(conn, &parsed.query, limit)?,
                    limit,
                );
            }
            Ok(results)
        }
    }
}

fn parse_search_shortcuts(query: &str, default_mode: SearchMode) -> ParsedSearch {
    let mut mode = default_mode;
    let mut exact = false;
    let mut remaining = query.trim();

    loop {
        let trimmed = remaining.trim_start();
        if !exact {
            if let Some(rest) = trimmed.strip_prefix('\'') {
                exact = true;
                remaining = rest;
                continue;
            }
        }
        if let Some((shortcut_mode, rest)) = parse_search_mode_shortcut(trimmed) {
            mode = shortcut_mode;
            remaining = rest;
            continue;
        }
        remaining = trimmed;
        break;
    }

    ParsedSearch {
        query: remaining.trim().to_string(),
        mode,
        exact,
    }
}

fn parse_search_mode_shortcut(value: &str) -> Option<(SearchMode, &str)> {
    let candidates = [
        ("bin:", SearchMode::Binary),
        ("binary:", SearchMode::Binary),
        ("main:", SearchMode::Binary),
        ("mainprogram:", SearchMode::Binary),
        ("program:", SearchMode::Binary),
        ("prog:", SearchMode::Binary),
        ("name:", SearchMode::Name),
        ("attr:", SearchMode::Name),
        ("desc:", SearchMode::Description),
        ("description:", SearchMode::Description),
        ("all:", SearchMode::All),
    ];

    for (prefix, mode) in candidates {
        if let Some(rest) = strip_prefix_ignore_ascii_case(value, prefix) {
            return Some((mode, rest));
        }
    }
    None
}

fn strip_prefix_ignore_ascii_case<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    let candidate = value.get(..prefix.len())?;
    if candidate.eq_ignore_ascii_case(prefix) {
        value.get(prefix.len()..)
    } else {
        None
    }
}

fn append_unique_by_attr(target: &mut Vec<PackageInfo>, extras: Vec<PackageInfo>, limit: usize) {
    if target.len() >= limit {
        return;
    }
    let mut seen: HashSet<String> = target.iter().map(|pkg| pkg.attr_path.clone()).collect();
    for pkg in extras {
        if seen.insert(pkg.attr_path.clone()) {
            target.push(pkg);
            if target.len() >= limit {
                break;
            }
        }
    }
}

fn search_packages_by_binary(
    conn: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<PackageInfo>, IndexError> {
    let mut stmt = conn.prepare(
        "SELECT p.attr_path, p.name, p.version, p.description, p.homepage, p.license, p.platforms, p.main_program, p.position, p.broken, p.insecure \
         FROM packages p \
         JOIN package_binaries b ON p.id = b.package_id \
         WHERE b.binary_name LIKE ?1 || '%' \
         ORDER BY b.binary_name \
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![query, limit as i64], |row| {
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

fn search_packages_by_binary_exact(
    conn: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<PackageInfo>, IndexError> {
    let mut stmt = conn.prepare(
        "SELECT p.attr_path, p.name, p.version, p.description, p.homepage, p.license, p.platforms, p.main_program, p.position, p.broken, p.insecure \
         FROM packages p \
         WHERE EXISTS (SELECT 1 FROM package_binaries b WHERE b.package_id = p.id AND LOWER(b.binary_name) = LOWER(?1)) \
         ORDER BY p.name \
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![query, limit as i64], |row| {
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

fn search_packages_by_name_exact(
    conn: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<PackageInfo>, IndexError> {
    let mut stmt = conn.prepare(
        "SELECT p.attr_path, p.name, p.version, p.description, p.homepage, p.license, p.platforms, p.main_program, p.position, p.broken, p.insecure \
         FROM packages p \
         WHERE LOWER(p.attr_path) = LOWER(?1) OR LOWER(p.name) = LOWER(?1) \
         ORDER BY CASE \
           WHEN LOWER(p.attr_path) = LOWER(?1) THEN 0 \
           WHEN LOWER(p.name) = LOWER(?1) THEN 1 \
           ELSE 2 \
         END, p.name \
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![query, limit as i64], |row| {
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

fn search_packages_by_description_exact(
    conn: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<PackageInfo>, IndexError> {
    let mut stmt = conn.prepare(
        "SELECT p.attr_path, p.name, p.version, p.description, p.homepage, p.license, p.platforms, p.main_program, p.position, p.broken, p.insecure \
         FROM packages p \
         WHERE p.description IS NOT NULL AND LOWER(p.description) = LOWER(?1) \
         ORDER BY p.name \
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![query, limit as i64], |row| {
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

fn search_packages_fts(
    conn: &Connection,
    query: &str,
    limit: usize,
    column: Option<&str>,
) -> Result<Vec<PackageInfo>, IndexError> {
    let fts_query = build_fts_query(query, column);
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

fn build_fts_query(query: &str, column: Option<&str>) -> String {
    let tokens: Vec<&str> = query
        .split_whitespace()
        .filter(|token| !token.is_empty())
        .collect();
    if tokens.is_empty() {
        return String::new();
    }
    match column {
        Some(column) => tokens
            .into_iter()
            .map(|token| format!("{}:{}*", column, token))
            .collect::<Vec<_>>()
            .join(" OR "),
        None => tokens
            .into_iter()
            .map(|token| format!("{}*", token))
            .collect::<Vec<_>>()
            .join(" OR "),
    }
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

#[cfg(test)]
mod tests {
    use crate::generate::{
        ingest_packages, init_db, list_packages, search_packages, search_packages_with_mode,
        NixPackage, SearchMode,
    };
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_db_path() -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock drift")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "mica-index-ingest-{}-{}.db",
            std::process::id(),
            suffix
        ))
    }

    fn pkg(attr_path: &str, name: &str, main_program: &str) -> NixPackage {
        NixPackage {
            attr_path: attr_path.to_string(),
            name: name.to_string(),
            version: Some("1.0.0".to_string()),
            description: None,
            homepage: None,
            license: None,
            platforms: None,
            main_program: Some(main_program.to_string()),
            position: None,
            broken: Some(false),
            insecure: Some(false),
        }
    }

    fn pkg_with_description(
        attr_path: &str,
        name: &str,
        main_program: &str,
        description: &str,
    ) -> NixPackage {
        NixPackage {
            description: Some(description.to_string()),
            ..pkg(attr_path, name, main_program)
        }
    }

    #[test]
    fn ingest_packages_replaces_removed_rows() {
        let path = temp_db_path();
        let mut conn = init_db(&path).expect("db init failed");

        let first = vec![pkg("alpha", "alpha", "alpha"), pkg("beta", "beta", "beta")];
        ingest_packages(&mut conn, &first).expect("first ingest failed");

        let second = vec![pkg("alpha", "alpha", "alpha")];
        ingest_packages(&mut conn, &second).expect("second ingest failed");

        let listed = list_packages(&conn, 10).expect("list failed");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].attr_path, "alpha");

        let alpha_hits = search_packages(&conn, "alpha", 10).expect("alpha search failed");
        assert_eq!(alpha_hits.len(), 1);
        assert_eq!(alpha_hits[0].attr_path, "alpha");

        let beta_hits = search_packages(&conn, "beta", 10).expect("beta search failed");
        assert!(beta_hits.is_empty());

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn search_shortcuts_support_exact_and_mode_override() {
        let path = temp_db_path();
        let mut conn = init_db(&path).expect("db init failed");

        let packages = vec![
            pkg_with_description("alpha", "alpha", "a", "exact alpha"),
            pkg_with_description("alphabet", "alphabet", "alpha", "alphabet package"),
            pkg_with_description("ripgrep", "ripgrep", "rg", "fast grep"),
        ];
        ingest_packages(&mut conn, &packages).expect("ingest failed");

        let fuzzy = search_packages_with_mode(&conn, "alpha", 10, SearchMode::Name)
            .expect("fuzzy search failed");
        assert!(
            fuzzy.iter().any(|pkg| pkg.attr_path == "alpha")
                && fuzzy.iter().any(|pkg| pkg.attr_path == "alphabet"),
            "expected prefix/fuzzy search to match both alpha and alphabet"
        );

        let exact = search_packages_with_mode(&conn, "'alpha", 10, SearchMode::Name)
            .expect("exact search failed");
        assert_eq!(exact.len(), 1);
        assert_eq!(exact[0].attr_path, "alpha");

        let bin_override = search_packages_with_mode(&conn, "bin:rg", 10, SearchMode::Name)
            .expect("binary override search failed");
        assert_eq!(bin_override.len(), 1);
        assert_eq!(bin_override[0].attr_path, "ripgrep");

        let bin_exact = search_packages_with_mode(&conn, "'bin:rg", 10, SearchMode::All)
            .expect("exact binary search failed");
        assert_eq!(bin_exact.len(), 1);
        assert_eq!(bin_exact[0].attr_path, "ripgrep");

        let bin_exact_miss = search_packages_with_mode(&conn, "'bin:r", 10, SearchMode::All)
            .expect("exact binary miss search failed");
        assert!(bin_exact_miss.is_empty());

        let _ = std::fs::remove_file(path);
    }
}
