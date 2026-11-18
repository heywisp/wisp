use syntect::parsing::Scope;

use crate::theme::Token;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    Paint(Token),
    Stop,
    Pass,
}

pub const SCOPES: [(&str, Rule); 39] = [
    ("comment", Rule::Paint(Token::SynComment)),
    ("constant.character", Rule::Paint(Token::SynString)),
    ("constant.language", Rule::Paint(Token::SynKeyword)),
    ("constant.numeric", Rule::Paint(Token::SynNumber)),
    (
        "constant.other.placeholder",
        Rule::Paint(Token::SynVariable),
    ),
    ("entity.name.class", Rule::Paint(Token::SynType)),
    ("entity.name.enum", Rule::Paint(Token::SynType)),
    ("entity.name.function", Rule::Paint(Token::SynFunction)),
    (
        "entity.name.function.preprocessor",
        Rule::Paint(Token::SynKeyword),
    ),
    ("entity.name.interface", Rule::Paint(Token::SynType)),
    ("entity.name.macro", Rule::Paint(Token::SynFunction)),
    ("entity.name.struct", Rule::Paint(Token::SynType)),
    ("entity.name.trait", Rule::Paint(Token::SynType)),
    ("entity.name.type", Rule::Paint(Token::SynType)),
    ("entity.name.union", Rule::Paint(Token::SynType)),
    ("entity.other.inherited-class", Rule::Paint(Token::SynType)),
    ("keyword", Rule::Paint(Token::SynKeyword)),
    ("keyword.control", Rule::Paint(Token::SynControl)),
    ("keyword.control.anchor.regexp", Rule::Pass),
    ("keyword.control.dockerfile", Rule::Paint(Token::SynKeyword)),
    ("keyword.operator", Rule::Pass),
    ("keyword.other.operator", Rule::Paint(Token::SynControl)),
    ("meta.embedded", Rule::Stop),
    ("meta.group.expansion", Rule::Stop),
    ("meta.interpolation", Rule::Stop),
    ("meta.template.expression", Rule::Stop),
    ("source", Rule::Stop),
    ("source.regexp", Rule::Pass),
    ("storage", Rule::Paint(Token::SynKeyword)),
    ("string", Rule::Paint(Token::SynString)),
    ("support.class", Rule::Paint(Token::SynType)),
    ("support.function", Rule::Paint(Token::SynFunction)),
    ("support.macro", Rule::Paint(Token::SynFunction)),
    ("support.type", Rule::Paint(Token::SynType)),
    ("support.variable", Rule::Paint(Token::SynVariable)),
    ("text", Rule::Stop),
    ("variable", Rule::Paint(Token::SynVariable)),
    ("variable.function", Rule::Paint(Token::SynFunction)),
    ("variable.language", Rule::Paint(Token::SynKeyword)),
];

pub struct ScopeRules {
    rules: Vec<(Scope, u32, Rule)>,
}

impl ScopeRules {
    pub fn new() -> ScopeRules {
        ScopeRules {
            rules: SCOPES
                .iter()
                .filter_map(|(name, rule)| {
                    let scope = Scope::new(name).ok()?;
                    Some((scope, scope.len(), *rule))
                })
                .collect(),
        }
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub fn token(&self, stack: &[Scope]) -> Option<Token> {
        for scope in stack.iter().rev() {
            let rule = self
                .rules
                .iter()
                .filter(|(prefix, _, _)| prefix.is_prefix_of(*scope))
                .max_by_key(|(_, len, _)| *len)
                .map(|(_, _, rule)| *rule);
            match rule {
                Some(Rule::Paint(token)) => return Some(token),
                Some(Rule::Stop) => return None,
                Some(Rule::Pass) | None => {}
            }
        }
        None
    }
}

impl Default for ScopeRules {
    fn default() -> ScopeRules {
        ScopeRules::new()
    }
}
