use std::collections::BTreeMap;

use wisp_protocol::context::{
    Category, CategoryEstimate, ContextReport, EstimateReason, FigureSource, ReportBasis,
};

use super::{apportion, slot, split, sum_rows};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::agent) struct CountKey {
    pub(in crate::agent) endpoint: String,
    pub(in crate::agent) model: String,
    pub(in crate::agent) effort: String,
    pub(in crate::agent) thinking: String,
    pub(in crate::agent) part: CountPart,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::agent) enum CountPart {
    Base,
    System {
        base: String,
    },
    SystemInstructions {
        base: String,
        instructions: String,
    },
    SystemInstructionsTools {
        base: String,
        instructions: String,
        tools: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::agent) enum Layer {
    Base,
    System,
    Instructions,
    Tools,
}

impl Layer {
    fn category(self) -> Option<Category> {
        match self {
            Self::Base => None,
            Self::System => Some(Category::SystemPrompt),
            Self::Instructions => Some(Category::Instructions),
            Self::Tools => Some(Category::Tools),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(in crate::agent) struct Fixed {
    pub(in crate::agent) system: String,
    pub(in crate::agent) instructions: String,
    pub(in crate::agent) tools: String,
}

impl Fixed {
    pub(in crate::agent) fn layers(&self) -> Vec<Layer> {
        let present = [
            (Layer::System, &self.system),
            (Layer::Instructions, &self.instructions),
            (Layer::Tools, &self.tools),
        ];
        let mut layers: Vec<Layer> = present
            .into_iter()
            .filter(|(_, text)| !text.is_empty())
            .map(|(layer, _)| layer)
            .collect();
        if !layers.is_empty() {
            layers.insert(0, Layer::Base);
        }
        layers
    }

    pub(in crate::agent) fn part(&self, layer: Layer) -> CountPart {
        match layer {
            Layer::Base => CountPart::Base,
            Layer::System => CountPart::System {
                base: self.system.clone(),
            },
            Layer::Instructions => CountPart::SystemInstructions {
                base: self.system.clone(),
                instructions: self.instructions.clone(),
            },
            Layer::Tools => CountPart::SystemInstructionsTools {
                base: self.system.clone(),
                instructions: self.instructions.clone(),
                tools: self.tools.clone(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::agent) enum Mode {
    Count,
    Estimate(EstimateReason),
}

#[derive(Debug, Clone)]
pub(in crate::agent) struct ReportInputs {
    pub(in crate::agent) report: ContextReport,
    pub(in crate::agent) estimates: Vec<CategoryEstimate>,
    pub(in crate::agent) layers: Vec<(Layer, CountKey)>,
    pub(in crate::agent) measured: Option<u64>,
    pub(in crate::agent) estimated_next: u64,
    pub(in crate::agent) split_estimate: bool,
    pub(in crate::agent) whole_extra: Option<u64>,
    pub(in crate::agent) mode: Mode,
}

impl ReportInputs {
    pub(in crate::agent) fn missing(&self, counts: &BTreeMap<CountKey, u64>) -> Vec<Layer> {
        if self.mode != Mode::Count {
            return vec![];
        }
        self.layers
            .iter()
            .filter(|(_, key)| !counts.contains_key(key))
            .map(|(layer, _)| *layer)
            .collect()
    }

    pub(in crate::agent) fn key(&self, layer: Layer) -> Option<&CountKey> {
        self.layers
            .iter()
            .find(|(l, _)| *l == layer)
            .map(|(_, key)| key)
    }

    pub(in crate::agent) fn counts_whole(&self) -> bool {
        self.mode == Mode::Count && self.whole_extra.is_some()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(in crate::agent) struct Results {
    pub(in crate::agent) settled: bool,
    pub(in crate::agent) whole: Option<u64>,
    pub(in crate::agent) failure: Option<String>,
    pub(in crate::agent) unsupported: bool,
}

impl Results {
    pub(in crate::agent) fn settled() -> Self {
        Self {
            settled: true,
            ..Self::default()
        }
    }
}

fn set_counted(rows: &mut Vec<CategoryEstimate>, category: Category, tokens: u64) {
    let row = slot(rows, category);
    let weights: Vec<u64> = row.items.iter().map(|i| i.tokens).collect();
    for (item, share) in row.items.iter_mut().zip(apportion(tokens, &weights)) {
        item.tokens = share;
    }
    row.tokens = tokens;
    row.source = FigureSource::Counted;
}

fn estimated(inputs: &ReportInputs, reason: EstimateReason) -> ContextReport {
    let rows = inputs.estimates.clone();
    let (categories, next) = if inputs.split_estimate {
        (split(rows, inputs.estimated_next), inputs.estimated_next)
    } else {
        (rows, inputs.estimated_next)
    };
    ContextReport {
        next,
        categories,
        basis: ReportBasis::Estimated { reason },
        ..inputs.report.clone()
    }
}

pub(in crate::agent) fn assemble(
    inputs: &ReportInputs,
    counts: &BTreeMap<CountKey, u64>,
    results: &Results,
) -> ContextReport {
    match &inputs.mode {
        Mode::Estimate(reason) => return estimated(inputs, reason.clone()),
        Mode::Count if results.unsupported => {
            return estimated(inputs, EstimateReason::NoCounter);
        }
        Mode::Count => {}
    }
    let mut rows = inputs.estimates.clone();
    let mut counted = 0usize;
    let mut every_part = true;
    for pair in inputs.layers.windows(2) {
        let [(_, before), (layer, key)] = pair else {
            continue;
        };
        let Some(category) = layer.category() else {
            continue;
        };
        match (counts.get(before), counts.get(key)) {
            (Some(before), Some(after)) => {
                set_counted(&mut rows, category, after.saturating_sub(*before));
                counted += 1;
            }
            _ => every_part = false,
        }
    }
    let exact = inputs.measured.or_else(|| {
        results
            .whole
            .zip(inputs.whole_extra)
            .map(|(whole, extra)| whole.saturating_add(extra))
    });
    let (categories, next) = match exact {
        Some(next) => (split(rows, next), next),
        None if inputs.split_estimate => {
            (split(rows, inputs.estimated_next), inputs.estimated_next)
        }
        None => {
            let next = sum_rows(&rows);
            (rows, next)
        }
    };
    let failure = || results.failure.clone().unwrap_or_default();
    let basis = if !results.settled {
        ReportBasis::Counting
    } else if every_part && exact.is_some() {
        ReportBasis::Counted
    } else if counted > 0 || results.whole.is_some() {
        ReportBasis::PartlyEstimated { reason: failure() }
    } else {
        ReportBasis::Estimated {
            reason: EstimateReason::Failed(failure()),
        }
    };
    ContextReport {
        next,
        categories,
        basis,
        ..inputs.report.clone()
    }
}
