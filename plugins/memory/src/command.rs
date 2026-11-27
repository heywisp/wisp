use wisp_plugin_api::commands::{CommandLine, CommandReply, CommandRow};
use wisp_plugin_api::host::Host;
use wisp_plugin_api::private_files::PrivateFileError;
use wisp_plugin_api::slots::{TextSpan, Tone};

use crate::note::{self, Scope};
use crate::store::{self, NoteStatus, Store};

pub(super) fn run(host: &dyn Host, args: &str) -> CommandReply {
    let mut words = args.split_whitespace();
    let lines = match (
        words.next(),
        words.next().and_then(Scope::parse_cli),
        words.next(),
        words.next(),
    ) {
        (None, _, _, _) => list(host),
        (Some(verb @ ("show" | "delete")), Some(scope), Some(name), None) if note::slug(name) => {
            if verb == "show" {
                show(host, scope, name)
            } else if !store::is_root(host) {
                vec![text(store::WORKER_REFUSAL)]
            } else {
                let result = Store::reading(host).forget(scope, name);
                vec![text(result.err().unwrap_or("Deleted."))]
            }
        }
        _ => vec![
            text("/memory"),
            text("/memory show <you|project> <name>"),
            text("/memory delete <you|project> <name>"),
        ],
    };
    CommandReply::Lines(lines)
}

fn list(host: &dyn Host) -> Vec<CommandLine> {
    let store = Store::reading(host);
    if store.project_probe == Err(PrivateFileError::Unavailable) {
        return vec![text(store::UNAVAILABLE)];
    }
    let mut lines = Vec::new();
    let mut total = 0;
    let mut hidden = false;
    for scope in Scope::ALL {
        let scan = match store.scan_scope(scope) {
            Ok(Some(scan)) => scan,
            Ok(None) => continue,
            Err(PrivateFileError::Unavailable) => return vec![text(store::UNAVAILABLE)],
            Err(_) => {
                hidden = true;
                continue;
            }
        };
        hidden |= scan.refused + scan.unreadable > 0;
        let notes: Vec<_> = scan
            .saved
            .into_iter()
            .filter_map(|file| {
                let name = file.note.id.name;
                let date = file.note.saved.to_string();
                let safe = store::safe(host, &name) && store::safe(host, &date);
                hidden |= !safe;
                safe.then_some((name, date))
            })
            .collect();
        if !notes.is_empty() {
            total += notes.len();
            lines.push(title(scope.label(), notes.len()));
            for (name, date) in notes {
                lines.extend([text(name), row(Vec::new(), date)]);
            }
        }
    }
    lines.insert(0, title("Memory", total));
    if total == 0 {
        lines.push(text("No notes."));
        if store.root {
            lines.push(text("Ask Wisp to remember a fact."));
        }
    }
    if hidden || store.project_probe.is_err() {
        lines.push(text(store::LOCAL_ONLY));
    }
    lines
}

fn show(host: &dyn Host, scope: Scope, name: &str) -> Vec<CommandLine> {
    if scope == Scope::You && !store::is_root(host) {
        return vec![text(store::NOT_FOUND)];
    }
    let store = Store::reading(host);
    let file = match store.inspect(scope, name) {
        NoteStatus::Valid(file) => file,
        NoteStatus::Absent => return vec![text(store::NOT_FOUND)],
        NoteStatus::Unavailable => return vec![text(store::UNAVAILABLE)],
        NoteStatus::Refused | NoteStatus::Unreadable => return vec![text(store::LOCAL_ONLY)],
    };
    let payload = crate::display::format_file(&file.raw);
    if !store::safe(host, &payload.join("\n"))
        || !payload.iter().all(|line| store::safe(host, line))
    {
        return vec![text(store::LOCAL_ONLY)];
    }
    let mut lines = vec![title("Memory", 0)];
    lines.extend(payload.into_iter().map(text));
    if store.root {
        let footer = format!("/memory delete {} {name}", scope.cli());
        if store::safe(host, &footer) {
            lines.extend([text(""), text(footer)]);
        }
    }
    lines
}

fn title(label: &str, count: usize) -> CommandLine {
    let mut label = span(label);
    label.bold = true;
    row(
        vec![label],
        if count == 0 {
            String::new()
        } else {
            count.to_string()
        },
    )
}

fn row(left: Vec<TextSpan>, right: String) -> CommandLine {
    let right = if right.is_empty() {
        Vec::new()
    } else {
        vec![span(right)]
    };
    CommandLine::Row(CommandRow { left, right })
}

fn span(text: impl Into<String>) -> TextSpan {
    TextSpan {
        text: text.into(),
        tone: Tone::Text,
        bold: false,
    }
}

fn text(value: impl Into<String>) -> CommandLine {
    let span = span(value);
    CommandLine::Text(if span.text.is_empty() {
        Vec::new()
    } else {
        vec![span]
    })
}
