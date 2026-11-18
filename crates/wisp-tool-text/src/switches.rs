pub const TURNED_OFF: &str = "Tools turned off: ";

pub const TURNED_ON: &str = "Tools turned on: ";

pub const REFUSED: &str = "this tool is turned off";

pub const BASE_LEAD: &str = "Wisp's base instructions changed since this conversation began. These replace the earlier base instructions in the system prompt:";

pub fn base_block(base: &str) -> String {
    format!("{BASE_LEAD}\n\n{base}")
}

pub fn is_base_block(text: &str) -> bool {
    text.starts_with(BASE_LEAD)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Switched {
    pub off: Vec<String>,
    pub on: Vec<String>,
}

impl Switched {
    pub fn is_empty(&self) -> bool {
        self.off.is_empty() && self.on.is_empty()
    }
}

fn line(lead: &str, names: &[String]) -> Option<String> {
    (!names.is_empty()).then(|| format!("{lead}{}.", names.join(", ")))
}

pub fn note(switched: &Switched) -> Option<String> {
    let lines: Vec<String> = [
        line(TURNED_OFF, &switched.off),
        line(TURNED_ON, &switched.on),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!lines.is_empty()).then(|| format!("{}\n", lines.join("\n")))
}

fn names(rest: &str) -> Option<Vec<String>> {
    let list = rest.strip_suffix('.')?;
    let names: Vec<String> = list.split(", ").map(str::to_owned).collect();
    names
        .iter()
        .all(|n| !n.is_empty() && !n.contains([',', ' ', '.']))
        .then_some(names)
}

pub fn parse(text: &str) -> Option<Switched> {
    let mut switched = Switched::default();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix(TURNED_OFF) {
            switched.off.extend(names(rest)?);
        } else {
            let rest = line.strip_prefix(TURNED_ON)?;
            switched.on.extend(names(rest)?);
        }
    }
    (!switched.is_empty()).then_some(switched)
}

#[cfg(test)]
mod tests;
