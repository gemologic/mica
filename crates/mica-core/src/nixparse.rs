use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use chrono::NaiveDate;

use crate::state::{NixBlocks, Pin, PinnedPackage, NIX_EXPR_PREFIX};

#[derive(Debug)]
pub enum ParseError {
    NotMicaManaged,
    MissingMarker(&'static str),
}

impl std::error::Error for ParseError {}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::NotMicaManaged => write!(f, "not a mica-managed nix file"),
            ParseError::MissingMarker(marker) => write!(f, "missing marker: {}", marker),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedNix {
    pub pin_section: String,
    pub pins_section: Option<String>,
    pub let_section: Option<String>,
    pub packages_section: String,
    pub packages_raw_section: Option<String>,
    pub scripts_section: Option<String>,
    pub env_section: String,
    pub env_raw_section: Option<String>,
    pub shell_hook_section: String,
    pub override_section: Option<String>,
    pub override_shellhook_section: Option<String>,
    pub override_merge_section: Option<String>,
    pub preamble: String,
    pub postamble: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedProfileNix {
    pub pins_section: String,
    pub paths_section: String,
    pub preamble: String,
    pub postamble: String,
}

pub fn parse_nix_file(content: &str) -> Result<ParsedNix, ParseError> {
    if !content.starts_with("# Managed by Mica") {
        return Err(ParseError::NotMicaManaged);
    }

    let preamble = extract_before_marker(content, "mica:pin:begin")?;
    let pin_section = extract_between_markers(content, "mica:pin:begin", "mica:pin:end")?;
    let pins_section =
        extract_between_markers_optional(content, "mica:pins:begin", "mica:pins:end")?;
    let let_section = extract_between_markers_optional(content, "mica:let:begin", "mica:let:end")?;
    let packages_section =
        extract_between_markers(content, "mica:packages:begin", "mica:packages:end")?;
    let packages_raw_section = extract_between_markers_optional(
        content,
        "mica:packages-raw:begin",
        "mica:packages-raw:end",
    )?;
    let scripts_section =
        extract_between_markers_optional(content, "mica:scripts:begin", "mica:scripts:end")?;
    let env_section = extract_between_markers(content, "mica:env:begin", "mica:env:end")?;
    let env_raw_section =
        extract_between_markers_optional(content, "mica:env-raw:begin", "mica:env-raw:end")?;
    let shell_hook_section =
        extract_between_markers(content, "mica:shellhook:begin", "mica:shellhook:end")?;
    let override_section =
        extract_between_markers_optional(content, "mica:override:begin", "mica:override:end")?;
    let override_shellhook_section = extract_between_markers_optional(
        content,
        "mica:override-shellhook:begin",
        "mica:override-shellhook:end",
    )?;
    let override_merge_section = extract_between_markers_optional(
        content,
        "mica:override-merge:begin",
        "mica:override-merge:end",
    )?;
    let postamble = extract_postamble(content)?;

    Ok(ParsedNix {
        pin_section,
        pins_section,
        let_section,
        packages_section,
        packages_raw_section,
        scripts_section,
        env_section,
        env_raw_section,
        shell_hook_section,
        override_section,
        override_shellhook_section,
        override_merge_section,
        preamble,
        postamble,
    })
}

pub fn parse_profile_nix(content: &str) -> Result<ParsedProfileNix, ParseError> {
    if !content.starts_with("# Managed by Mica") {
        return Err(ParseError::NotMicaManaged);
    }

    let preamble = extract_before_marker(content, "mica:pins:begin")?;
    let pins_section = extract_between_markers(content, "mica:pins:begin", "mica:pins:end")?;
    let paths_section = extract_between_markers(content, "mica:paths:begin", "mica:paths:end")?;
    let postamble = extract_after_marker(content, "mica:paths:end")?;

    Ok(ParsedProfileNix {
        pins_section,
        paths_section,
        preamble,
        postamble,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum StateParseError {
    #[error("nix parse error: {0}")]
    Nix(#[from] ParseError),
    #[error("missing pin url in nix")]
    MissingPinUrl,
    #[error("missing pin sha256 in nix")]
    MissingPinSha,
    #[error("missing pin rev in nix")]
    MissingPinRev,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedProjectState {
    pub pin: Pin,
    pub pins: BTreeMap<String, Pin>,
    pub packages: Vec<String>,
    pub pinned: BTreeMap<String, PinnedPackage>,
    pub env: BTreeMap<String, String>,
    pub shell_hook: Option<String>,
    pub presets: Vec<String>,
    pub nix: NixBlocks,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedProfileState {
    pub pin: Pin,
    pub packages: Vec<String>,
    pub pinned: BTreeMap<String, PinnedPackage>,
}

pub fn parse_project_state_from_nix(content: &str) -> Result<ParsedProjectState, StateParseError> {
    let parsed = parse_nix_file(content)?;
    let pin = parse_pin_section(&parsed.pin_section)?;
    let (mut pins, pins_block) = parse_pin_args(parsed.pins_section.as_deref());
    let (packages, presets, pinned, pinned_pin_names) =
        parse_package_list(&parsed.packages_section, &pins);
    for name in pinned_pin_names {
        pins.remove(&name);
    }
    let env = parse_env_section(&parsed.env_section);
    let shell_hook = parse_shell_hook(&parsed.shell_hook_section);
    Ok(ParsedProjectState {
        pin,
        pins,
        packages,
        pinned,
        env,
        shell_hook,
        presets,
        nix: NixBlocks {
            let_block: normalize_optional_block(parsed.let_section),
            pins: normalize_optional_block(pins_block),
            packages_raw: normalize_optional_block(parsed.packages_raw_section),
            scripts: normalize_optional_block(parsed.scripts_section),
            env_raw: normalize_optional_block(parsed.env_raw_section),
            override_attrs: normalize_optional_block(parsed.override_section),
            override_merge: normalize_optional_block(parsed.override_merge_section),
            override_shell_hook: parse_override_shellhook(parsed.override_shellhook_section),
        },
    })
}

pub fn parse_profile_state_from_nix(content: &str) -> Result<ParsedProfileState, StateParseError> {
    let parsed = parse_profile_nix(content)?;
    let pin = parse_pin_section(&parsed.pins_section)?;
    let pinned_pins = parse_profile_pins(&parsed.pins_section);
    let (packages, pinned) = parse_profile_paths(&parsed.paths_section, &pinned_pins);
    Ok(ParsedProfileState {
        pin,
        packages,
        pinned,
    })
}

fn parse_pin_section(section: &str) -> Result<Pin, StateParseError> {
    let name = find_attr_value(section, "name").filter(|value| !value.trim().is_empty());
    let url = find_attr_value(section, "url").ok_or(StateParseError::MissingPinUrl)?;
    let sha256 = find_attr_value(section, "sha256").ok_or(StateParseError::MissingPinSha)?;
    let rev = extract_rev_from_url(&url).ok_or(StateParseError::MissingPinRev)?;
    Ok(Pin {
        name,
        url: trim_archive_url(&url),
        rev,
        sha256,
        branch: String::new(),
        updated: NaiveDate::from_ymd_opt(1970, 1, 1).unwrap(),
    })
}

fn parse_pin_args(section: Option<&str>) -> (BTreeMap<String, Pin>, Option<String>) {
    let mut pins = BTreeMap::new();
    let mut raw_lines = Vec::new();
    let mut current: Option<(String, Vec<String>)> = None;
    let mut current_name: Option<String> = None;
    let mut current_url: Option<String> = None;
    let mut current_sha: Option<String> = None;

    let Some(section) = section else {
        return (pins, None);
    };

    for line in section.lines() {
        let trimmed = line.trim();
        if current.is_none() {
            if trimmed.starts_with(',') && trimmed.contains("? import (fetchTarball") {
                let rest = trimmed.trim_start_matches(',').trim();
                if let Some((name, _)) = rest.split_once('?') {
                    let name = name.trim().to_string();
                    current = Some((name, vec![line.to_string()]));
                    current_name = None;
                    current_url = None;
                    current_sha = None;
                    continue;
                }
            }
            raw_lines.push(line.to_string());
            continue;
        }

        if let Some((_, lines)) = current.as_mut() {
            lines.push(line.to_string());
        }
        if let Some(rest) = trimmed.strip_prefix("url =") {
            current_url = Some(trim_quotes(rest.trim_end_matches(';').trim()));
        }
        if let Some(rest) = trimmed.strip_prefix("sha256 =") {
            current_sha = Some(trim_quotes(rest.trim_end_matches(';').trim()));
        }
        if let Some(rest) = trimmed.strip_prefix("name =") {
            current_name = Some(trim_quotes(rest.trim_end_matches(';').trim()));
        }

        if trimmed.contains("})") {
            if let Some((name, lines)) = current.take() {
                if let (Some(url), Some(sha256)) = (current_url.take(), current_sha.take()) {
                    if let Some(rev) = extract_rev_from_url(&url) {
                        pins.insert(
                            name,
                            Pin {
                                name: current_name.take().filter(|value| !value.trim().is_empty()),
                                url: trim_archive_url(&url),
                                rev,
                                sha256,
                                branch: String::new(),
                                updated: NaiveDate::from_ymd_opt(1970, 1, 1).unwrap(),
                            },
                        );
                        continue;
                    }
                }
                raw_lines.extend(lines);
            }
        }
    }

    if let Some((_, lines)) = current.take() {
        raw_lines.extend(lines);
    }

    let raw_block = raw_lines.join("\n");
    if raw_block.trim().is_empty() {
        (pins, None)
    } else {
        (pins, Some(raw_block))
    }
}

fn parse_profile_pins(section: &str) -> BTreeMap<String, Pin> {
    let mut pins = BTreeMap::new();
    let mut current: Option<String> = None;
    let mut current_name: Option<String> = None;
    let mut current_url: Option<String> = None;
    let mut current_sha: Option<String> = None;

    for line in section.lines() {
        let trimmed = line.trim();
        if current.is_none() {
            if trimmed.starts_with("pkgs-") && trimmed.contains("= import (fetchTarball") {
                if let Some((name, _)) = trimmed.split_once('=') {
                    current = Some(name.trim().to_string());
                    current_name = None;
                    current_url = None;
                    current_sha = None;
                }
            }
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("name =") {
            current_name = Some(trim_quotes(rest.trim_end_matches(';').trim()));
        }
        if let Some(rest) = trimmed.strip_prefix("url =") {
            current_url = Some(trim_quotes(rest.trim_end_matches(';').trim()));
        }
        if let Some(rest) = trimmed.strip_prefix("sha256 =") {
            current_sha = Some(trim_quotes(rest.trim_end_matches(';').trim()));
        }

        if trimmed.starts_with("})") {
            if let (Some(name), Some(url), Some(sha256)) =
                (current.take(), current_url.take(), current_sha.take())
            {
                let rev = extract_rev_from_url(&url).unwrap_or_default();
                let pin = Pin {
                    name: current_name.take(),
                    url: trim_archive_url(&url),
                    rev,
                    sha256,
                    branch: String::new(),
                    updated: NaiveDate::from_ymd_opt(1970, 1, 1).unwrap(),
                };
                pins.insert(name, pin);
            }
        }
    }

    pins
}

fn find_attr_value(section: &str, key: &str) -> Option<String> {
    for line in section.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let needle = format!("{} =", key);
        if let Some(rest) = line.strip_prefix(&needle) {
            let value = rest.trim().trim_end_matches(';').trim();
            return Some(trim_quotes(value));
        }
    }
    None
}

fn trim_quotes(value: &str) -> String {
    value.trim_matches('"').trim_matches('\'').to_string()
}

fn extract_rev_from_url(url: &str) -> Option<String> {
    let archive = url.split("/archive/").nth(1)?;
    Some(archive.trim_end_matches(".tar.gz").to_string())
}

fn trim_archive_url(url: &str) -> String {
    if let Some((base, _)) = url.split_once("/archive/") {
        return base.to_string();
    }
    url.to_string()
}

fn normalize_package_name(value: &str) -> String {
    value
        .strip_prefix("nixos.")
        .or_else(|| value.strip_prefix("pkgs."))
        .unwrap_or(value)
        .to_string()
}

fn parse_package_list(
    section: &str,
    pins: &BTreeMap<String, Pin>,
) -> (
    Vec<String>,
    Vec<String>,
    BTreeMap<String, PinnedPackage>,
    BTreeSet<String>,
) {
    let mut packages = Vec::new();
    let mut presets = Vec::new();
    let mut pinned = BTreeMap::new();
    let mut pinned_pin_names = BTreeSet::new();
    let mut in_raw_block = false;
    for line in section.lines() {
        let trimmed = line.trim();
        if trimmed.contains("mica:packages-raw:begin") {
            in_raw_block = true;
            continue;
        }
        if trimmed.contains("mica:packages-raw:end") {
            in_raw_block = false;
            continue;
        }
        if in_raw_block {
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with('#') {
            if let Some(name) = trimmed.strip_prefix("# Preset: ") {
                presets.push(name.trim().to_string());
            }
            continue;
        }
        if trimmed.contains("packages =")
            || trimmed.starts_with("tools =")
            || trimmed.contains("= with pkgs; [")
            || trimmed == "["
            || trimmed == "];"
            || trimmed.starts_with("] ++")
        {
            continue;
        }
        let raw_item = trimmed.trim_end_matches(',').trim();
        let (item, comment) = match raw_item.split_once('#') {
            Some((left, right)) => (left.trim(), Some(right.trim().to_string())),
            None => (raw_item, None),
        };
        if item.starts_with('#') || item.is_empty() {
            continue;
        }
        if let Some((prefix, attr)) = item.split_once('.') {
            if prefix.starts_with("pkgs-") {
                if let Some(pin) = pins.get(prefix) {
                    let name = normalize_package_name(attr);
                    let version = comment.unwrap_or_else(|| "unknown".to_string());
                    pinned.insert(
                        name,
                        PinnedPackage {
                            version,
                            pin: pin.clone(),
                        },
                    );
                    pinned_pin_names.insert(prefix.to_string());
                    continue;
                }
            }
        }
        packages.push(normalize_package_name(item));
    }
    (packages, presets, pinned, pinned_pin_names)
}

fn parse_profile_paths(
    section: &str,
    pins: &BTreeMap<String, Pin>,
) -> (Vec<String>, BTreeMap<String, PinnedPackage>) {
    let mut packages = Vec::new();
    let mut pinned = BTreeMap::new();
    for line in section.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed.contains("paths =") || trimmed == "[" || trimmed == "];" {
            continue;
        }
        let raw_item = trimmed.trim_end_matches(',').trim();
        let (item, comment) = match raw_item.split_once('#') {
            Some((left, right)) => (left.trim(), Some(right.trim().to_string())),
            None => (raw_item, None),
        };
        if let Some((prefix, attr)) = item.split_once('.') {
            if prefix.starts_with("pkgs-") {
                if let Some(pin) = pins.get(prefix) {
                    let name = normalize_package_name(attr);
                    let version = comment.unwrap_or_else(|| "unknown".to_string());
                    pinned.insert(
                        name,
                        PinnedPackage {
                            version,
                            pin: pin.clone(),
                        },
                    );
                    continue;
                }
            }
        }
        if let Some(attr) = item.strip_prefix("pkgs.") {
            packages.push(attr.to_string());
        }
    }
    (packages, pinned)
}

fn parse_env_section(section: &str) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    let mut in_raw_block = false;
    for line in section.lines() {
        let trimmed = line.trim();
        if trimmed.contains("mica:env-raw:begin") {
            in_raw_block = true;
            continue;
        }
        if trimmed.contains("mica:env-raw:end") {
            in_raw_block = false;
            continue;
        }
        if in_raw_block {
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = trimmed.split_once('=') {
            let key = key.trim();
            let value = value.trim().trim_end_matches(';').trim();
            env.insert(key.to_string(), parse_env_value(value));
        }
    }
    env
}

fn parse_env_value(value: &str) -> String {
    let trimmed = value.trim();
    if is_quoted_nix_expression(trimmed) {
        return format!("{}{}", NIX_EXPR_PREFIX, trimmed);
    }
    if is_indented_string_literal(trimmed) {
        return format!("{}{}", NIX_EXPR_PREFIX, trimmed);
    }
    if is_unquoted_nix_expression(trimmed) {
        return format!("{}{}", NIX_EXPR_PREFIX, trimmed);
    }
    trim_quotes(trimmed)
}

fn is_quoted_nix_expression(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.len() >= 2
        && trimmed.starts_with('\"')
        && trimmed.ends_with('\"')
        && contains_unescaped_interpolation(trimmed)
}

fn contains_unescaped_interpolation(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut idx = 0usize;
    while idx + 1 < bytes.len() {
        if bytes[idx] == b'$' && bytes[idx + 1] == b'{' && (idx == 0 || bytes[idx - 1] != b'\\') {
            return true;
        }
        idx += 1;
    }
    false
}

fn is_indented_string_literal(value: &str) -> bool {
    value.len() >= 4 && value.starts_with("''") && value.ends_with("''")
}

fn is_unquoted_nix_expression(value: &str) -> bool {
    !(value.is_empty()
        || (value.starts_with('\"') && value.ends_with('\"'))
        || (value.starts_with("''") && value.ends_with("''")))
}

fn parse_shell_hook(section: &str) -> Option<String> {
    let mut lines = section.lines();
    let mut in_hook = false;
    let mut buffer = String::new();
    for line in lines.by_ref() {
        if line.contains("shellHook = ''") {
            in_hook = true;
            continue;
        }
        if in_hook {
            if line.contains("'';") {
                break;
            }
            buffer.push_str(line);
            buffer.push('\n');
        }
    }
    if buffer.is_empty() {
        None
    } else {
        Some(buffer)
    }
}

fn parse_override_shellhook(section: Option<String>) -> Option<String> {
    let raw = section?;
    let hook = parse_shell_hook(&raw)?;
    normalize_optional_block(Some(hook))
}

fn marker_line_bounds(content: &str, marker: &'static str) -> Result<(usize, usize), ParseError> {
    let idx = content
        .find(marker)
        .ok_or(ParseError::MissingMarker(marker))?;
    Ok((line_start(content, idx), line_end(content, idx)))
}

fn marker_line_bounds_optional(content: &str, marker: &'static str) -> Option<(usize, usize)> {
    content
        .find(marker)
        .map(|idx| (line_start(content, idx), line_end(content, idx)))
}

fn line_start(content: &str, idx: usize) -> usize {
    content[..idx].rfind('\n').map(|pos| pos + 1).unwrap_or(0)
}

fn line_end(content: &str, idx: usize) -> usize {
    content[idx..]
        .find('\n')
        .map(|offset| idx + offset + 1)
        .unwrap_or_else(|| content.len())
}

fn extract_between_markers(
    content: &str,
    start: &'static str,
    end: &'static str,
) -> Result<String, ParseError> {
    let (_, start_line_end) = marker_line_bounds(content, start)?;
    let (end_line_start, _) = marker_line_bounds(content, end)?;

    if end_line_start <= start_line_end {
        return Ok(String::new());
    }

    Ok(strip_trailing_marker_stub(
        &content[start_line_end..end_line_start],
    ))
}

fn extract_between_markers_optional(
    content: &str,
    start: &'static str,
    end: &'static str,
) -> Result<Option<String>, ParseError> {
    let start_bounds = marker_line_bounds_optional(content, start);
    let end_bounds = marker_line_bounds_optional(content, end);
    match (start_bounds, end_bounds) {
        (None, None) => Ok(None),
        (Some(_), None) => Err(ParseError::MissingMarker(end)),
        (None, Some(_)) => Err(ParseError::MissingMarker(start)),
        (Some((_, start_line_end)), Some((end_line_start, _))) => {
            if end_line_start <= start_line_end {
                return Ok(Some(String::new()));
            }
            Ok(Some(strip_trailing_marker_stub(
                &content[start_line_end..end_line_start],
            )))
        }
    }
}

fn extract_before_marker(content: &str, marker: &'static str) -> Result<String, ParseError> {
    let (line_start, _) = marker_line_bounds(content, marker)?;
    Ok(strip_trailing_marker_stub(&content[..line_start]))
}

fn extract_after_marker(content: &str, marker: &'static str) -> Result<String, ParseError> {
    let (_, line_end) = marker_line_bounds(content, marker)?;
    Ok(strip_trailing_marker_stub(&content[line_end..]))
}

fn extract_postamble(content: &str) -> Result<String, ParseError> {
    if content.contains("mica:override-merge:end") {
        let post = extract_after_marker(content, "mica:override-merge:end")?;
        Ok(strip_scripts_merge(&post))
    } else if content.contains("mica:override-shellhook:end") {
        extract_after_marker(content, "mica:override-shellhook:end")
    } else if content.contains("mica:override:end") {
        extract_after_marker(content, "mica:override:end")
    } else {
        extract_after_marker(content, "mica:shellhook:end")
    }
}

fn strip_scripts_merge(value: &str) -> String {
    let mut out = Vec::new();
    let mut seen_nonempty = false;
    let mut skipped = false;
    for line in value.lines() {
        if !seen_nonempty {
            if line.trim().is_empty() {
                out.push(line.to_string());
                continue;
            }
            seen_nonempty = true;
            if line.trim() == "// { inherit scripts; }" {
                skipped = true;
                continue;
            }
        }
        out.push(line.to_string());
    }
    if skipped {
        out.join("\n")
    } else {
        value.to_string()
    }
}

fn strip_trailing_marker_stub(value: &str) -> String {
    let mut out = value.to_string();
    let trimmed = out.trim();
    if trimmed.is_empty() || trimmed.chars().all(|ch| ch == '#') {
        return String::new();
    }
    loop {
        let last_newline = match out.rfind('\n') {
            Some(idx) => idx,
            None => return out,
        };
        let tail = &out[last_newline + 1..];
        let trimmed = tail.trim();
        if trimmed.is_empty() || trimmed.chars().all(|ch| ch == '#') {
            out.truncate(last_newline);
            continue;
        }
        return out;
    }
}

fn normalize_optional_block(block: Option<String>) -> Option<String> {
    let raw = block?;
    let trimmed = raw.trim_matches('\n');
    if trimmed.trim().is_empty() {
        return None;
    }

    let mut min_indent: Option<usize> = None;
    for line in trimmed.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let indent = line
            .as_bytes()
            .iter()
            .take_while(|byte| byte.is_ascii_whitespace())
            .count();
        min_indent = Some(match min_indent {
            Some(current) => current.min(indent),
            None => indent,
        });
    }

    let indent = min_indent.unwrap_or(0);
    let mut normalized = String::new();
    for line in trimmed.lines() {
        let line_bytes = line.as_bytes();
        let offset = indent.min(line_bytes.len());
        normalized.push_str(&line[offset..]);
        normalized.push('\n');
    }

    Some(normalized.trim_end_matches('\n').to_string())
}

#[cfg(test)]
mod tests {
    use crate::nixparse::parse_env_section;
    use crate::state::NIX_EXPR_PREFIX;

    #[test]
    fn parse_env_section_keeps_interpolated_nix_string_expressions() {
        let env = parse_env_section(
            r#"
            MICA_A = "${pkgs.path}/meme";
            "#,
        );
        let expected = format!("{}\"${{pkgs.path}}/meme\"", NIX_EXPR_PREFIX);

        assert_eq!(
            env.get("MICA_A").map(String::as_str),
            Some(expected.as_str())
        );
    }

    #[test]
    fn parse_env_section_trims_plain_quoted_values() {
        let env = parse_env_section(
            r#"
            MICA_A = "hello";
            "#,
        );

        assert_eq!(env.get("MICA_A").map(String::as_str), Some("hello"));
    }

    #[test]
    fn parse_env_section_keeps_unquoted_nix_expressions() {
        let env = parse_env_section(
            r#"
            MICA_A = pkgs.path + "/meme";
            "#,
        );
        let expected = format!("{}pkgs.path + \"/meme\"", NIX_EXPR_PREFIX);

        assert_eq!(
            env.get("MICA_A").map(String::as_str),
            Some(expected.as_str())
        );
    }

    #[test]
    fn parse_env_section_keeps_escaped_interpolation_as_plain_string() {
        let env = parse_env_section(
            r#"
            MICA_A = "\${HOME}/mica";
            "#,
        );

        assert_eq!(
            env.get("MICA_A").map(String::as_str),
            Some("\\${HOME}/mica")
        );
    }
}
