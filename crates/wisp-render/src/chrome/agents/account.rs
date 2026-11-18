use crate::chrome::account::Chip;
use crate::chrome::summary::estimated;
use crate::transcript::copy;
use wisp_protocol::{AccountSpend, Marked, marked_amount};

pub(crate) use crate::chrome::account::THIS_CHAT;
pub(in crate::chrome) const TOTAL: &str = "total";

fn unspent_subscription(spend: &AccountSpend, chip: Option<&Chip>) -> bool {
    spend.usd <= 0.0 && chip.is_some_and(|c| c.billed.is_subscription())
}

pub(super) fn cell_cost(spend: &AccountSpend, priced: bool, chip: Option<&Chip>) -> String {
    if !priced && spend.usd <= 0.0 {
        return copy::NOT_PRICED.to_owned();
    }
    let estimate = estimated(spend) || unspent_subscription(spend, chip);
    let marked = marked_amount(&copy::usd(spend.usd), estimate, spend.lower_bound);
    format!("{}{}{}", marked.lead, marked.amount, marked.tail)
}

pub(in crate::chrome) fn tree_amount(total: &AccountSpend) -> Marked {
    marked_amount(&copy::usd(total.usd), estimated(total), total.lower_bound)
}

pub(super) fn total_label(amount: &str) -> String {
    format!("{TOTAL} {amount}")
}
