use wisp_instructions::{
    Diagnostic, FILE_NAME, Loaded, Problem, Scope, Source, estimate_tokens, group, scope_name,
};
use wisp_plugin_api::tools::{DoctorCell, DoctorRow};

fn kib(bytes: u64) -> String {
    #[allow(
        clippy::cast_precision_loss,
        reason = "a size shown to one decimal place"
    )]
    let value = bytes as f64 / 1024.0;
    format!("{value:.1} KiB")
}

struct Row {
    label: &'static str,
    path: String,
    size: Option<u64>,
    status: String,
}

fn problems(loaded: &Loaded) -> impl Iterator<Item = &Problem> {
    loaded.summary.diagnostics.iter().filter_map(|d| match d {
        Diagnostic::Problem(problem) => Some(problem),
        Diagnostic::Large(_) => None,
    })
}

fn status_of(loaded: &Loaded, scope: Scope, exists: bool) -> String {
    let chosen = loaded.chosen == Some(scope);
    let candidate = loaded.summary.candidates.iter().find(|c| c.scope == scope);
    let shown = candidate.map(|c| c.display_path.as_str());
    if chosen {
        if let Some(problem) = problems(loaded).find(|p| p.source.as_deref() == shown) {
            return problem.message.clone();
        }
        let own_text_empty = loaded
            .files
            .iter()
            .find(|f| f.imported_by.is_none())
            .is_none_or(|f| f.text.trim().is_empty());
        if own_text_empty && loaded.summary.imports.is_empty() {
            return "empty: loads nothing, and no other file is used".to_owned();
        }
        return "loads".to_owned();
    }
    if !exists {
        return "missing".to_owned();
    }
    match (loaded.source, scope, loaded.chosen) {
        (Source::None, _, _) => "not used: --instructions none".to_owned(),
        (_, Scope::Default, Some(Scope::Project)) => {
            format!("not used: the project has {FILE_NAME}")
        }
        (Source::Default, Scope::Project, _) => "not used: --instructions default".to_owned(),
        _ => "not used".to_owned(),
    }
}

pub(crate) fn rows(loaded: &Loaded) -> Vec<DoctorRow> {
    let mut rows: Vec<Row> = Vec::new();
    let mut shown_paths: Vec<String> = Vec::new();
    let imports = |rows: &mut Vec<Row>, shown_paths: &mut Vec<String>| {
        for import in &loaded.summary.imports {
            let from = import.imported_by.as_deref().unwrap_or(FILE_NAME);
            let status = if import.external {
                format!("from {from}, outside the repository")
            } else {
                format!("from {from}")
            };
            shown_paths.push(import.display_path.clone());
            rows.push(Row {
                label: "import",
                path: import.display_path.clone(),
                size: Some(import.bytes),
                status,
            });
        }
    };
    for candidate in &loaded.summary.candidates {
        let scope = match candidate.scope {
            Scope::Project => Scope::Project,
            _ => Scope::Default,
        };
        let exists = candidate.bytes.is_some()
            || problems(loaded)
                .any(|p| p.source.as_deref() == Some(candidate.display_path.as_str()));
        shown_paths.push(candidate.display_path.clone());
        rows.push(Row {
            label: scope_name(candidate.scope),
            path: candidate.display_path.clone(),
            size: candidate.bytes,
            status: status_of(loaded, scope, exists),
        });
        if loaded.chosen == Some(scope) {
            imports(&mut rows, &mut shown_paths);
        }
    }
    for problem in problems(loaded) {
        let Some(source) = &problem.source else {
            continue;
        };
        if shown_paths.contains(source) {
            continue;
        }
        rows.push(Row {
            label: "import",
            path: source.clone(),
            size: None,
            status: problem.message.clone(),
        });
    }
    let mut out: Vec<DoctorRow> = rows
        .into_iter()
        .map(|r| DoctorRow {
            key: r.label.to_owned(),
            cells: vec![
                DoctorCell::Text(r.path),
                DoctorCell::Bytes(r.size),
                DoctorCell::Text(r.status),
            ],
        })
        .collect();
    let total = loaded.total_bytes();
    if loaded.summary.chosen.is_some() {
        out.push(DoctorRow {
            key: "total".to_owned(),
            cells: vec![DoctorCell::Text(format!(
                "{}, about {} tokens",
                kib(total),
                group(estimate_tokens(total))
            ))],
        });
    }
    if loaded.chosen.is_none()
        && loaded.summary.candidates.iter().all(|c| c.bytes.is_none())
        && let Some(default) = loaded
            .summary
            .candidates
            .iter()
            .find(|c| c.scope == Scope::Default)
    {
        out.push(DoctorRow {
            key: "hint".to_owned(),
            cells: vec![DoctorCell::Text(format!(
                "write {} for instructions in every project without its own {FILE_NAME}",
                default.display_path
            ))],
        });
    }
    out
}
