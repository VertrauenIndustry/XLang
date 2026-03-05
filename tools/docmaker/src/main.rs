use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use rayon::prelude::*;

#[derive(Debug, Clone, Default)]
struct ProjectInfo {
    name: String,
    version: String,
    tagline: String,
}

#[derive(Debug, Clone)]
struct CliCommand {
    name: String,
    usage: String,
    description: String,
}

#[derive(Debug, Clone)]
struct SyntaxTopic {
    title: String,
    description: String,
    example: String,
}

#[derive(Debug, Clone)]
struct Builtin {
    name: String,
    signature: String,
    returns: String,
    category: String,
    runtime_only: bool,
    description: String,
    example: String,
}

#[derive(Debug, Clone, Default)]
struct Manifest {
    project: ProjectInfo,
    commands: Vec<CliCommand>,
    syntax: Vec<SyntaxTopic>,
    builtins: Vec<Builtin>,
    notes: Vec<String>,
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let manifest_path = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "docs/doc_manifest.xdocs".to_string());
    let out_dir = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| "docs/generated".to_string());

    let manifest_text = fs::read_to_string(&manifest_path)
        .map_err(|e| format!("failed to read manifest '{}': {e}", manifest_path))?;
    let mut manifest = parse_manifest(&manifest_text)?;
    manifest.builtins.sort_by(|a, b| a.name.cmp(&b.name));

    let out_path = PathBuf::from(out_dir);
    fs::create_dir_all(&out_path)
        .map_err(|e| format!("failed to create output dir '{}': {e}", out_path.display()))?;

    let index = render_index(&manifest);
    let reference = render_reference(&manifest);
    let builtins = render_builtins(&manifest);
    let cli = render_cli(&manifest);

    let files = vec![
        (out_path.join("INDEX.md"), index),
        (out_path.join("REFERENCE.md"), reference),
        (out_path.join("BUILTINS.md"), builtins),
        (out_path.join("CLI.md"), cli),
    ];

    files
        .par_iter()
        .try_for_each(|(path, content)| write_file(path, content))?;

    println!(
        "generated docs into {} using parallel writer (rayon)",
        out_path.display()
    );
    Ok(())
}

fn parse_manifest(text: &str) -> Result<Manifest, String> {
    let mut out = Manifest::default();
    let mut section = "project".to_string();

    for (idx, raw) in text.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].trim().to_lowercase();
            continue;
        }
        match section.as_str() {
            "project" => parse_project_line(&mut out.project, line, line_no)?,
            "commands" => out.commands.push(parse_command_line(line, line_no)?),
            "syntax" => out.syntax.push(parse_syntax_line(line, line_no)?),
            "builtins" => out.builtins.push(parse_builtin_line(line, line_no)?),
            "notes" => out.notes.push(unescape(line)),
            _ => {
                return Err(format!(
                    "line {line_no}: unknown section '[{section}]' in manifest"
                ));
            }
        }
    }
    Ok(out)
}

fn parse_project_line(project: &mut ProjectInfo, line: &str, line_no: usize) -> Result<(), String> {
    let Some((key, value)) = line.split_once('=') else {
        return Err(format!("line {line_no}: expected key=value in [project]"));
    };
    let key = key.trim();
    let value = unescape(value.trim());
    match key {
        "name" => project.name = value,
        "version" => project.version = value,
        "tagline" => project.tagline = value,
        _ => return Err(format!("line {line_no}: unknown project key '{key}'")),
    }
    Ok(())
}

fn parse_command_line(line: &str, line_no: usize) -> Result<CliCommand, String> {
    let parts = split_fields(line);
    if parts.len() != 3 {
        return Err(format!(
            "line {line_no}: commands row expects 3 fields (name|usage|description)"
        ));
    }
    Ok(CliCommand {
        name: unescape(parts[0].trim()),
        usage: unescape(parts[1].trim()),
        description: unescape(parts[2].trim()),
    })
}

fn parse_syntax_line(line: &str, line_no: usize) -> Result<SyntaxTopic, String> {
    let parts = split_fields(line);
    if parts.len() != 3 {
        return Err(format!(
            "line {line_no}: syntax row expects 3 fields (title|description|example)"
        ));
    }
    Ok(SyntaxTopic {
        title: unescape(parts[0].trim()),
        description: unescape(parts[1].trim()),
        example: unescape(parts[2].trim()),
    })
}

fn parse_builtin_line(line: &str, line_no: usize) -> Result<Builtin, String> {
    let parts = split_fields(line);
    if parts.len() != 7 {
        return Err(format!(
            "line {line_no}: builtins row expects 7 fields (name|signature|returns|category|runtime_only|description|example)"
        ));
    }
    let runtime_only = matches!(
        parts[4].trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes"
    );
    Ok(Builtin {
        name: unescape(parts[0].trim()),
        signature: unescape(parts[1].trim()),
        returns: unescape(parts[2].trim()),
        category: unescape(parts[3].trim()),
        runtime_only,
        description: unescape(parts[5].trim()),
        example: unescape(parts[6].trim()),
    })
}

fn split_fields(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut escape = false;
    for ch in line.chars() {
        if escape {
            cur.push(ch);
            escape = false;
            continue;
        }
        if ch == '\\' {
            escape = true;
            continue;
        }
        if ch == '|' {
            out.push(cur);
            cur = String::new();
            continue;
        }
        cur.push(ch);
    }
    out.push(cur);
    out
}

fn unescape(raw: &str) -> String {
    raw.replace("\\n", "\n")
}

fn render_index(m: &Manifest) -> String {
    format!(
        "# {} Docs\n\n{}\n\n- Version: `{}`\n- Generated by `tools/docmaker` (3rd-party parallel engine: `rayon`)\n\n## Pages\n- [Reference](REFERENCE.md)\n- [Builtins](BUILTINS.md)\n- [CLI](CLI.md)\n",
        m.project.name, m.project.tagline, m.project.version
    )
}

fn render_reference(m: &Manifest) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# {} Reference\n\n{}\n\n",
        m.project.name, m.project.tagline
    ));
    out.push_str("## Syntax Topics\n");
    for s in &m.syntax {
        out.push_str(&format!(
            "\n### {}\n{}\n\n```x\n{}\n```\n",
            s.title, s.description, s.example
        ));
    }
    out.push_str("\n## CLI Commands\n");
    for c in &m.commands {
        out.push_str(&format!("- `{}`: {}\n", c.usage, c.description));
    }
    out.push_str("\n## Builtin Categories\n");
    let grouped = group_builtins(&m.builtins);
    for (category, items) in grouped {
        out.push_str(&format!("\n### {}\n", category));
        for b in items {
            out.push_str(&format!(
                "- `{}`: `{}` -> `{}`\n",
                b.name, b.signature, b.returns
            ));
        }
    }
    out.push_str("\n## Notes\n");
    for n in &m.notes {
        out.push_str(&format!("- {}\n", n));
    }
    out
}

fn render_builtins(m: &Manifest) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {} Builtins\n", m.project.name));
    let grouped = group_builtins(&m.builtins);
    for (category, items) in grouped {
        out.push_str(&format!("\n## {}\n", category));
        for b in items {
            out.push_str(&format!(
                "\n### `{}`\n- Signature: `{}`\n- Returns: `{}`\n- Runtime-only: `{}`\n- Description: {}\n- Example:\n```x\n{}\n```\n",
                b.name,
                b.signature,
                b.returns,
                if b.runtime_only { "yes" } else { "no" },
                b.description,
                b.example
            ));
        }
    }
    out
}

fn render_cli(m: &Manifest) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {} CLI\n", m.project.name));
    for c in &m.commands {
        out.push_str(&format!(
            "\n## `{}`\n- Usage: `{}`\n- Description: {}\n",
            c.name, c.usage, c.description
        ));
    }
    out
}

fn group_builtins(items: &[Builtin]) -> BTreeMap<String, Vec<Builtin>> {
    let mut out: BTreeMap<String, Vec<Builtin>> = BTreeMap::new();
    for b in items {
        out.entry(b.category.clone()).or_default().push(b.clone());
    }
    for v in out.values_mut() {
        v.sort_by(|a, b| a.name.cmp(&b.name));
    }
    out
}

fn write_file(path: &Path, content: &str) -> Result<(), String> {
    fs::write(path, content).map_err(|e| format!("failed to write '{}': {e}", path.display()))
}
