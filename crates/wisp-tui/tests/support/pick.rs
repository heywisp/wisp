macro_rules! pick {
    ($items:expr, $pattern:pat => $value:expr) => {
        $items
            .into_iter()
            .filter_map(|item| match item {
                $pattern => Some($value),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
}

pub(crate) use pick;
