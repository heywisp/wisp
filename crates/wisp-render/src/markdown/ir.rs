#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Marks {
    pub emphasis: bool,
    pub strong: bool,
    pub strike: bool,
    pub code: bool,
    pub link: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Inline {
    Text { text: String, marks: Marks },
    Target(String),
    Break,
}

pub type Inlines = Vec<Inline>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Table {
    pub align: Vec<Align>,
    pub head: Vec<Inlines>,
    pub rows: Vec<Vec<Inlines>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Block {
    Paragraph(Inlines),
    Heading {
        level: u8,
        text: Inlines,
    },
    List {
        start: Option<u64>,
        items: Vec<Item>,
    },
    Quote(Vec<Block>),
    Code {
        lang: Option<String>,
        text: String,
    },
    Table(Table),
    Rule,
    Definitions(Vec<String>),
    Plain(Vec<String>),
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Item {
    pub spaced: bool,
    pub blocks: Vec<Child>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Child {
    pub spaced: bool,
    pub block: Block,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Parsed {
    pub block: Block,
    pub source: std::ops::Range<usize>,
}
