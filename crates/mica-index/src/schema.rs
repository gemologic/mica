pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS packages (
    id INTEGER PRIMARY KEY,
    attr_path TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    version TEXT,
    description TEXT,
    homepage TEXT,
    license TEXT,
    platforms TEXT,
    main_program TEXT,
    position TEXT,
    broken INTEGER DEFAULT 0,
    insecure INTEGER DEFAULT 0
);

CREATE VIRTUAL TABLE IF NOT EXISTS packages_fts USING fts5(
    attr_path,
    name,
    description,
    content='packages',
    content_rowid='id'
);

CREATE TRIGGER IF NOT EXISTS packages_ai AFTER INSERT ON packages BEGIN
    INSERT INTO packages_fts(rowid, attr_path, name, description)
    VALUES (new.id, new.attr_path, new.name, new.description);
END;

CREATE TABLE IF NOT EXISTS package_binaries (
    id INTEGER PRIMARY KEY,
    package_id INTEGER NOT NULL REFERENCES packages(id),
    binary_name TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_binaries_name ON package_binaries(binary_name);

CREATE TABLE IF NOT EXISTS meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;
