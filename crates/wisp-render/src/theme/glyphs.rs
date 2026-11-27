#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GlyphSet {
    Unicode,
    AmbiguousWide,
    Ascii,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Glyphs {
    pub set: GlyphSet,
    pub prompt: &'static str,
    pub user: &'static str,
    pub wisp: &'static str,
    pub caret: &'static str,
    pub comet: &'static [&'static str],
    pub running: &'static str,
    pub done: &'static str,
    pub failed: &'static str,
    pub waiting: &'static str,
    pub stopped: &'static str,
    pub needs_you: &'static str,
    pub moved: &'static str,
    pub woke: &'static str,
    pub typed: &'static str,
    pub mail: &'static str,
    pub agents: &'static str,
    pub rail: &'static str,
    pub approval_rail: &'static str,
    pub collapsed: &'static str,
    pub expanded: &'static str,
    pub separator: &'static str,
    pub bullet: &'static str,
    pub more: &'static str,
    pub newline: &'static str,
    pub wrap: &'static str,
    pub arrow: &'static str,
    pub back: &'static str,
    pub rewound: &'static str,
    pub minus: &'static str,
    pub tee: &'static str,
    pub elbow: &'static str,
    pub pipe: &'static str,
    pub rule: &'static str,
    pub spark: Option<&'static [&'static str]>,
    pub cell_full: &'static str,
    pub cell_free: &'static str,
    pub cell_reserve: &'static str,
    pub band: &'static str,
    pub hollow: &'static str,
    pub mark: &'static str,
    pub absent: &'static str,
    pub dot_working: &'static str,
    pub dot_waiting: &'static str,
    pub dot_stopped: &'static str,
    pub wire_drop: &'static str,
    pub wire_bar: &'static str,
    pub wire_first: &'static str,
    pub wire_last: &'static str,
    pub wire_join: &'static str,
    pub wire_cross: &'static str,
    pub more_up: &'static str,
    pub more_down: &'static str,
    pub more_left: &'static str,
    pub more_right: &'static str,
}

pub const UNICODE: Glyphs = Glyphs {
    set: GlyphSet::Unicode,
    prompt: "\u{203A}",
    user: "\u{203A}",
    wisp: "\u{2836}",
    caret: "\u{2836}",
    comet: &[
        "\u{2803}", "\u{2809}", "\u{2818}", "\u{2830}", "\u{28A0}", "\u{28C0}", "\u{2844}",
        "\u{2806}",
    ],
    running: "\u{25CC}",
    done: "\u{2713}",
    failed: "\u{2A2F}",
    waiting: "\u{25E6}",
    stopped: "\u{25E6}",
    needs_you: "\u{25C9}",
    moved: "\u{21D7}",
    woke: "\u{21BB}",
    typed: "\u{2386}",
    mail: "\u{25C7}",
    agents: "\u{29C9}",
    rail: "\u{254E}",
    approval_rail: "\u{2503}",
    collapsed: "\u{25B8}",
    expanded: "\u{25BE}",
    separator: "\u{00B7}",
    bullet: "\u{2219}",
    more: "\u{2026}",
    newline: "\u{23CE}",
    wrap: "\u{21B3}",
    arrow: "\u{2192}",
    back: "\u{2190}",
    rewound: "\u{21B6}",
    minus: "\u{2212}",
    tee: "\u{251C}\u{2500}",
    elbow: "\u{2514}\u{2500}",
    pipe: "\u{2502} ",
    rule: "\u{2500}",
    spark: None,
    cell_full: "\u{28FF}",
    cell_free: "\u{28C0}",
    cell_reserve: "\u{286A}",
    band: "\u{258C}",
    hollow: "\u{25CB}",
    mark: "\u{25CF}",
    absent: "\u{2014}",
    dot_working: "\u{25CF}",
    dot_waiting: "\u{25CB}",
    dot_stopped: "\u{25A0}",
    wire_drop: "\u{2847}",
    wire_bar: "\u{2812}",
    wire_first: "\u{2856}",
    wire_last: "\u{2846}",
    wire_join: "\u{2813}",
    wire_cross: "\u{2857}",
    more_up: "\u{25B4}",
    more_down: "\u{25BE}",
    more_left: "\u{25C2}",
    more_right: "\u{25B8}",
};

pub const AMBIGUOUS_WIDE: Glyphs = Glyphs {
    set: GlyphSet::AmbiguousWide,
    approval_rail: "|",
    mail: "@",
    separator: "\u{2219}",
    more: "...",
    arrow: "->",
    back: "<-",
    rewound: "<-",
    tee: "|-",
    elbow: "`-",
    pipe: "| ",
    rule: "-",
    band: ">",
    hollow: "o",
    mark: "*",
    absent: "-",
    dot_working: "@",
    dot_waiting: "o",
    dot_stopped: "=",
    ..UNICODE
};

pub const ASCII: Glyphs = Glyphs {
    set: GlyphSet::Ascii,
    prompt: ">",
    user: ">",
    wisp: "*",
    caret: "_",
    comet: &["-", "\\", "|", "/"],
    running: "o",
    done: "+",
    failed: "x",
    waiting: ".",
    stopped: "-",
    needs_you: "!",
    moved: "^",
    woke: "~",
    typed: ">",
    mail: "@",
    agents: "=",
    rail: ":",
    approval_rail: "|",
    collapsed: ">",
    expanded: "v",
    separator: "-",
    bullet: "-",
    more: "...",
    newline: "\\n",
    wrap: "|",
    arrow: "->",
    back: "<-",
    rewound: "<-",
    minus: "-",
    tee: "|-",
    elbow: "`-",
    pipe: "| ",
    rule: "-",
    spark: Some(&[".", ":", "-", "=", "+", "*", "#"]),
    cell_full: "#",
    cell_free: ".",
    cell_reserve: "/",
    band: ">",
    hollow: "o",
    mark: "*",
    absent: "-",
    dot_working: "@",
    dot_waiting: "o",
    dot_stopped: "=",
    wire_drop: "|",
    wire_bar: "-",
    wire_first: ".",
    wire_last: ".",
    wire_join: "'",
    wire_cross: "|",
    more_up: "^",
    more_down: "v",
    more_left: "<",
    more_right: ">",
};

impl Glyphs {
    pub const fn of(set: GlyphSet) -> Glyphs {
        match set {
            GlyphSet::Unicode => UNICODE,
            GlyphSet::AmbiguousWide => AMBIGUOUS_WIDE,
            GlyphSet::Ascii => ASCII,
        }
    }

    fn fold_mapping(&self) -> [(&str, &str); 7] {
        [
            (UNICODE.separator, self.separator),
            (UNICODE.minus, self.minus),
            (
                "—",
                if self.set == GlyphSet::Ascii {
                    "-"
                } else {
                    "—"
                },
            ),
            (UNICODE.more, self.more),
            (UNICODE.arrow, self.arrow),
            (UNICODE.back, self.back),
            (UNICODE.done, self.done),
        ]
    }

    pub fn fold(&self, text: &str) -> String {
        self.fold_mapping()
            .into_iter()
            .fold(text.to_owned(), |text, (from, to)| text.replace(from, to))
    }

    pub fn all(&self) -> Vec<(&'static str, &'static str)> {
        let mut out = vec![
            ("prompt", self.prompt),
            ("user", self.user),
            ("wisp", self.wisp),
            ("caret", self.caret),
            ("running", self.running),
            ("done", self.done),
            ("failed", self.failed),
            ("waiting", self.waiting),
            ("stopped", self.stopped),
            ("needs_you", self.needs_you),
            ("moved", self.moved),
            ("woke", self.woke),
            ("typed", self.typed),
            ("mail", self.mail),
            ("agents", self.agents),
            ("rail", self.rail),
            ("approval_rail", self.approval_rail),
            ("collapsed", self.collapsed),
            ("expanded", self.expanded),
            ("separator", self.separator),
            ("bullet", self.bullet),
            ("more", self.more),
            ("newline", self.newline),
            ("wrap", self.wrap),
            ("arrow", self.arrow),
            ("back", self.back),
            ("minus", self.minus),
            ("tee", self.tee),
            ("elbow", self.elbow),
            ("pipe", self.pipe),
            ("rule", self.rule),
            ("cell_full", self.cell_full),
            ("cell_free", self.cell_free),
            ("cell_reserve", self.cell_reserve),
            ("band", self.band),
            ("hollow", self.hollow),
            ("mark", self.mark),
            ("absent", self.absent),
            ("dot_working", self.dot_working),
            ("dot_waiting", self.dot_waiting),
            ("dot_stopped", self.dot_stopped),
            ("wire_drop", self.wire_drop),
            ("wire_bar", self.wire_bar),
            ("wire_first", self.wire_first),
            ("wire_last", self.wire_last),
            ("wire_join", self.wire_join),
            ("wire_cross", self.wire_cross),
            ("more_up", self.more_up),
            ("more_down", self.more_down),
            ("more_left", self.more_left),
            ("more_right", self.more_right),
        ];
        out.extend(self.comet.iter().map(|g| ("comet", *g)));
        out.extend(self.spark.into_iter().flatten().map(|g| ("spark", *g)));
        out
    }
}

#[cfg(test)]
#[test]
fn terminal_fold_sources_match_renderer_mapping() {
    use std::collections::BTreeSet;
    let glyphs = [UNICODE, AMBIGUOUS_WIDE, ASCII];
    let sources = glyphs
        .iter()
        .flat_map(Glyphs::fold_mapping)
        .map(|(source, _)| source)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        sources,
        wisp_tool_text::terminal::FOLD_SOURCES
            .iter()
            .copied()
            .collect()
    );
}
